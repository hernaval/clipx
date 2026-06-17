use std::path::Path;

use anyhow::{bail, Context};
use argon2::Argon2;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::Clip;
use crate::storage::Storage;

const MAGIC: &[u8; 5] = b"CLIPX";
const FORMAT_VERSION: u8 = 1;
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 24;

#[derive(Serialize, Deserialize)]
struct BackupPayload {
    version: u8,
    created_at: DateTime<Utc>,
    device_name: String,
    clip_count: usize,
    clips: Vec<BackupClip>,
}

#[derive(Serialize, Deserialize)]
struct BackupClip {
    id: Uuid,
    content: String,
    format: String,
    created_at: DateTime<Utc>,
    pin_slot: Option<u8>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug)]
pub struct BackupResult {
    pub clip_count: usize,
    pub file_size: u64,
}

#[derive(Debug)]
pub struct RestoreResult {
    pub total_in_backup: usize,
    pub imported: usize,
    pub skipped: usize,
}

fn derive_key(password: &str, salt: &[u8; SALT_LEN]) -> anyhow::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("KDF failed: {e}"))?;
    Ok(key)
}

pub async fn export_backup(
    storage: &Storage,
    device_name: &str,
    password: &str,
    path: &Path,
) -> anyhow::Result<BackupResult> {
    let clips = storage.get_all_clips().await?;
    let clip_count = clips.len();

    let payload = BackupPayload {
        version: FORMAT_VERSION,
        created_at: Utc::now(),
        device_name: device_name.to_string(),
        clip_count,
        clips: clips
            .into_iter()
            .map(|c| BackupClip {
                id: c.id,
                content: c.content,
                format: c.format,
                created_at: c.created_at,
                pin_slot: c.pin_slot,
                category: c.category,
            })
            .collect(),
    };

    let json = serde_json::to_vec(&payload)?;
    let compressed = zstd::encode_all(json.as_slice(), 3)?;

    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut salt);
    rand::rng().fill_bytes(&mut nonce_bytes);

    let key = derive_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, compressed.as_ref())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    let plaintext_len = (compressed.len() as u32).to_le_bytes();

    let mut out = Vec::with_capacity(5 + 1 + SALT_LEN + NONCE_LEN + 4 + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&plaintext_len);
    out.extend_from_slice(&ciphertext);

    tokio::fs::write(path, &out).await.context("Failed to write backup file")?;

    let file_size = out.len() as u64;
    Ok(BackupResult { clip_count, file_size })
}

pub async fn import_backup(
    storage: &Storage,
    user_id: Uuid,
    device_id: Uuid,
    password: &str,
    path: &Path,
) -> anyhow::Result<RestoreResult> {
    let data = tokio::fs::read(path).await.context("Failed to read backup file")?;

    let min_len = 5 + 1 + SALT_LEN + NONCE_LEN + 4;
    if data.len() < min_len {
        bail!("File is too small to be a valid backup");
    }
    if &data[0..5] != MAGIC {
        bail!("Not a ClipX backup file");
    }
    if data[5] != FORMAT_VERSION {
        bail!("Unsupported backup version: {}", data[5]);
    }

    let salt: [u8; SALT_LEN] = data[6..6 + SALT_LEN].try_into().unwrap();
    let nonce_bytes: [u8; NONCE_LEN] = data[6 + SALT_LEN..6 + SALT_LEN + NONCE_LEN]
        .try_into()
        .unwrap();
    let ciphertext = &data[6 + SALT_LEN + NONCE_LEN + 4..];

    let key = derive_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let nonce = XNonce::from_slice(&nonce_bytes);
    let compressed = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Decryption failed — wrong password or corrupted file"))?;

    let json = zstd::decode_all(compressed.as_slice()).context("Decompression failed")?;
    let payload: BackupPayload =
        serde_json::from_slice(&json).context("Invalid backup payload")?;

    let total = payload.clips.len();
    let mut imported = 0usize;

    for bc in payload.clips {
        let clip = Clip {
            id: bc.id,
            user_id,
            device_id,
            content: bc.content,
            format: bc.format,
            e2e: false,
            created_at: bc.created_at,
            pin_slot: bc.pin_slot,
            category: bc.category,
        };
        match storage.insert_clip(&clip).await {
            Ok(rows) if rows > 0 => imported += 1,
            Ok(_) => {} // duplicate, skipped by INSERT OR IGNORE
            Err(_) => {}
        }
    }

    Ok(RestoreResult {
        total_in_backup: total,
        imported,
        skipped: total - imported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_storage() -> (Storage, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::new(db_path.to_str().unwrap()).await.unwrap();
        (storage, dir)
    }

    #[tokio::test]
    async fn round_trip_export_import() {
        let (storage, dir) = test_storage().await;
        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        for i in 0..5 {
            let clip = Clip {
                id: Uuid::new_v4(),
                user_id,
                device_id,
                content: format!("clip content {i}"),
                format: "text".into(),
                e2e: false,
                created_at: Utc::now(),
                pin_slot: if i == 0 { Some(1) } else { None },
                category: None,
            };
            storage.insert_clip(&clip).await.unwrap();
        }

        let backup_path = dir.path().join("test.clipx");
        let result = export_backup(&storage, "TestPC", "hunter2", &backup_path)
            .await
            .unwrap();
        assert_eq!(result.clip_count, 5);
        assert!(result.file_size > 0);

        let (storage2, _dir2) = test_storage().await;
        let restore = import_backup(&storage2, user_id, device_id, "hunter2", &backup_path)
            .await
            .unwrap();
        assert_eq!(restore.total_in_backup, 5);
        assert_eq!(restore.imported, 5);
        assert_eq!(restore.skipped, 0);

        let clips = storage2.get_all_clips().await.unwrap();
        assert_eq!(clips.len(), 5);
        assert!(clips.iter().any(|c| c.pin_slot == Some(1)));
    }

    #[tokio::test]
    async fn wrong_password_fails() {
        let (storage, dir) = test_storage().await;
        let clip = Clip {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            content: "secret".into(),
            format: "text".into(),
            e2e: false,
            created_at: Utc::now(),
            pin_slot: None,
            category: None,
        };
        storage.insert_clip(&clip).await.unwrap();

        let backup_path = dir.path().join("test.clipx");
        export_backup(&storage, "PC", "correct", &backup_path)
            .await
            .unwrap();

        let (storage2, _dir2) = test_storage().await;
        let err = import_backup(
            &storage2,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "wrong",
            &backup_path,
        )
        .await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("wrong password"));
    }

    #[tokio::test]
    async fn duplicate_clips_skipped() {
        let (storage, dir) = test_storage().await;
        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        let clip = Clip {
            id: Uuid::new_v4(),
            user_id,
            device_id,
            content: "hello".into(),
            format: "text".into(),
            e2e: false,
            created_at: Utc::now(),
            pin_slot: None,
            category: None,
        };
        storage.insert_clip(&clip).await.unwrap();

        let backup_path = dir.path().join("test.clipx");
        export_backup(&storage, "PC", "pw", &backup_path)
            .await
            .unwrap();

        let restore = import_backup(&storage, user_id, device_id, "pw", &backup_path)
            .await
            .unwrap();
        assert_eq!(restore.total_in_backup, 1);
        assert_eq!(restore.skipped, 1);
        assert_eq!(restore.imported, 0);
    }
}
