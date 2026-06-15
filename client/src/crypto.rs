use anyhow::{anyhow, Result};
use base64::Engine;
use crypto_box::{
    aead::{Aead, AeadCore},
    ChaChaBox, PublicKey, SecretKey,
};
use serde::{Deserialize, Serialize};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPair {
    pub public_key: String,
    pub private_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedClip {
    pub v: u8,
    pub recipients: Vec<EncryptedKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKey {
    pub device_id: String,
    pub ephemeral_pub: String,
    pub nonce: String,
    pub ciphertext: String,
}

pub fn generate_keypair() -> Result<KeyPair> {
    let mut rng = rand_core::OsRng;
    let secret = SecretKey::generate(&mut rng);
    let public = secret.public_key();
    Ok(KeyPair {
        public_key: B64.encode(public.as_bytes()),
        private_key: B64.encode(secret.to_bytes()),
    })
}

fn parse_pub(b64: &str) -> Result<PublicKey> {
    let bytes = B64.decode(b64).map_err(|e| anyhow!("invalid base64 pubkey: {e}"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("pubkey length != 32"))?;
    Ok(PublicKey::from(arr))
}

fn parse_secret(b64: &str) -> Result<SecretKey> {
    let bytes = B64.decode(b64).map_err(|e| anyhow!("invalid base64 secret: {e}"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("secret length != 32"))?;
    Ok(SecretKey::from(arr))
}

/// Returns `(device_id, public_key_base64)` for each intended recipient.
pub fn encrypt_for_recipients(
    plaintext: &str,
    recipients: &[(String, String)],
) -> Result<EncryptedClip> {
    if recipients.is_empty() {
        return Err(anyhow!("no recipients"));
    }
    let mut rng = rand_core::OsRng;
    let mut entries = Vec::with_capacity(recipients.len());

    for (device_id, pub_b64) in recipients {
        let recipient_pub = match parse_pub(pub_b64) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Skipping recipient {device_id}: {e}");
                continue;
            }
        };
        let eph_secret = SecretKey::generate(&mut rng);
        let eph_pub = eph_secret.public_key();
        let aead = ChaChaBox::new(&recipient_pub, &eph_secret);
        let nonce = ChaChaBox::generate_nonce(&mut rng);
        let ciphertext = aead
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow!("encrypt failed: {e}"))?;

        entries.push(EncryptedKey {
            device_id: device_id.clone(),
            ephemeral_pub: B64.encode(eph_pub.as_bytes()),
            nonce: B64.encode(nonce.as_slice()),
            ciphertext: B64.encode(&ciphertext),
        });
    }

    if entries.is_empty() {
        return Err(anyhow!("no usable recipient pubkeys"));
    }

    Ok(EncryptedClip { v: 1, recipients: entries })
}

/// Attempt to decrypt; returns `Ok(plaintext)` if our device_id is in the
/// recipient list and the AEAD verifies.
pub fn decrypt_for_device(
    encrypted: &EncryptedClip,
    my_device_id: &str,
    my_private_b64: &str,
) -> Result<String> {
    let entry = encrypted
        .recipients
        .iter()
        .find(|e| e.device_id == my_device_id)
        .ok_or_else(|| anyhow!("clip not addressed to this device"))?;
    let sender_pub = parse_pub(&entry.ephemeral_pub)?;
    let secret = parse_secret(my_private_b64)?;
    let aead = ChaChaBox::new(&sender_pub, &secret);
    let nonce_bytes = B64
        .decode(&entry.nonce)
        .map_err(|e| anyhow!("bad nonce: {e}"))?;
    let nonce_arr: &crypto_box::Nonce =
        crypto_box::Nonce::from_slice(&nonce_bytes);
    let ct = B64
        .decode(&entry.ciphertext)
        .map_err(|e| anyhow!("bad ciphertext: {e}"))?;
    let pt = aead
        .decrypt(nonce_arr, ct.as_slice())
        .map_err(|e| anyhow!("decrypt failed: {e}"))?;
    Ok(String::from_utf8(pt)?)
}

/// Serialize an EncryptedClip as a single base64 string suitable for storage
/// in the `Clip.content` field. Wrapping the JSON in base64 keeps the field
/// dense and unambiguous as it crosses LAN / cloud transports.
pub fn pack(enc: &EncryptedClip) -> Result<String> {
    let json = serde_json::to_string(enc)?;
    Ok(format!("e2e:1:{}", B64.encode(json.as_bytes())))
}

/// Reverse of `pack`. Returns `None` if `s` does not look E2E-packed.
pub fn unpack(s: &str) -> Option<EncryptedClip> {
    let rest = s.strip_prefix("e2e:1:")?;
    let bytes = B64.decode(rest).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Try to decrypt an inbound clip's content in place. Returns:
///  - `Ok(true)`  — the clip was E2E and we successfully decrypted it.
///  - `Ok(false)` — the clip was plaintext (no e2e: prefix); nothing changed.
///  - `Err(_)`    — the clip was E2E but decryption failed (wrong recipient or corrupt).
pub fn try_decrypt_in_place(
    content: &mut String,
    my_device_id: &str,
    my_private_b64: &str,
) -> Result<bool> {
    let Some(enc) = unpack(content) else {
        return Ok(false);
    };
    let plaintext = decrypt_for_device(&enc, my_device_id, my_private_b64)?;
    *content = plaintext;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let kp = generate_keypair().unwrap();
        assert_eq!(B64.decode(&kp.public_key).unwrap().len(), 32);
        assert_eq!(B64.decode(&kp.private_key).unwrap().len(), 32);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let alice = generate_keypair().unwrap();
        let bob = generate_keypair().unwrap();
        let plaintext = "Hello, ClipX!";
        let enc = encrypt_for_recipients(
            plaintext,
            &[("bob-device".into(), bob.public_key.clone())],
        )
        .unwrap();
        let decrypted = decrypt_for_device(&enc, "bob-device", &bob.private_key).unwrap();
        assert_eq!(decrypted, plaintext);

        // Alice can't decrypt — wrong device id
        assert!(decrypt_for_device(&enc, "alice", &alice.private_key).is_err());
    }

    #[test]
    fn test_pack_unpack() {
        let enc = EncryptedClip {
            v: 1,
            recipients: vec![EncryptedKey {
                device_id: "x".into(),
                ephemeral_pub: "AAA".into(),
                nonce: "BBB".into(),
                ciphertext: "CCC".into(),
            }],
        };
        let s = pack(&enc).unwrap();
        let back = unpack(&s).unwrap();
        assert_eq!(back.v, enc.v);
        assert_eq!(back.recipients.len(), 1);
        assert!(unpack("plaintext").is_none());
    }
}
