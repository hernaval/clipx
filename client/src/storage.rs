use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use uuid::Uuid;

use crate::models::{Clip, PeerDevice, QueueEntry};

pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    pub async fn new(db_path: &str) -> anyhow::Result<Self> {
        let url = format!("sqlite:{db_path}?mode=rwc");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let storage = Self { pool };
        storage.migrate().await?;
        Ok(storage)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clips (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                content TEXT NOT NULL,
                format TEXT NOT NULL DEFAULT 'text',
                e2e INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                public_key TEXT,
                paired_at TEXT NOT NULL,
                revoked INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS queue (
                id TEXT PRIMARY KEY,
                clip_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_clips_user_id ON clips(user_id);
            CREATE INDEX IF NOT EXISTS idx_clips_created_at ON clips(created_at);
            CREATE INDEX IF NOT EXISTS idx_queue_status ON queue(status);
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS peer_devices (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                public_key TEXT NOT NULL DEFAULT '',
                ip TEXT NOT NULL,
                port INTEGER NOT NULL,
                session_token TEXT NOT NULL DEFAULT '',
                paired_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Idempotent ADD COLUMN — ignore "duplicate column" error on re-runs.
        let _ = sqlx::query("ALTER TABLE clips ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE clips ADD COLUMN pin_slot INTEGER DEFAULT NULL")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE clips ADD COLUMN category TEXT DEFAULT NULL")
            .execute(&self.pool)
            .await;
        Ok(())
    }

    pub async fn insert_clip(&self, clip: &Clip) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO clips (id, user_id, device_id, content, format, e2e, created_at, pinned, pin_slot, category) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(clip.id.to_string())
        .bind(clip.user_id.to_string())
        .bind(clip.device_id.to_string())
        .bind(&clip.content)
        .bind(&clip.format)
        .bind(clip.e2e as i32)
        .bind(clip.created_at.to_rfc3339())
        .bind(clip.is_pinned() as i32)
        .bind(clip.pin_slot.map(|v| v as i32))
        .bind(clip.category.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn get_recent_clips(&self, _user_id: Uuid, limit: i64) -> anyhow::Result<Vec<Clip>> {
        // user_id filter intentionally dropped: this is a single-user local store.
        // Filtering by current settings.user_id breaks history across re-bootstraps,
        // when the server hands out a fresh user UUID.
        let rows = sqlx::query_as::<_, ClipRow>(
            "SELECT id, user_id, device_id, content, format, e2e, created_at, pinned, pin_slot, category FROM clips ORDER BY CASE WHEN pin_slot IS NOT NULL THEN 0 ELSE 1 END, pin_slot ASC, created_at DESC LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.try_into()).collect()
    }

    pub async fn get_all_clips(&self) -> anyhow::Result<Vec<Clip>> {
        let rows = sqlx::query_as::<_, ClipRow>(
            "SELECT id, user_id, device_id, content, format, e2e, created_at, pinned, pin_slot, category FROM clips ORDER BY created_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into()).collect()
    }

    #[allow(dead_code)]
    pub async fn search_clips(&self, _user_id: Uuid, query: &str, limit: i64) -> anyhow::Result<Vec<Clip>> {
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let rows = sqlx::query_as::<_, ClipRow>(
            "SELECT id, user_id, device_id, content, format, e2e, created_at, pinned, pin_slot, category FROM clips WHERE content LIKE ? ESCAPE '\\' ORDER BY CASE WHEN pin_slot IS NOT NULL THEN 0 ELSE 1 END, pin_slot ASC, created_at DESC LIMIT ?"
        )
        .bind(like)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into()).collect()
    }

    pub async fn set_pin_slot(&self, clip_id: Uuid, slot: Option<u8>) -> anyhow::Result<()> {
        sqlx::query("UPDATE clips SET pin_slot = ?, pinned = ? WHERE id = ?")
            .bind(slot.map(|v| v as i32))
            .bind(slot.is_some() as i32)
            .bind(clip_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn clear_pin_slot(&self, slot: u8) -> anyhow::Result<()> {
        sqlx::query("UPDATE clips SET pin_slot = NULL, pinned = 0 WHERE pin_slot = ?")
            .bind(slot as i32)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_category(&self, clip_id: Uuid, category: Option<&str>) -> anyhow::Result<()> {
        sqlx::query("UPDATE clips SET category = ? WHERE id = ?")
            .bind(category)
            .bind(clip_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_clip(&self, clip_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM clips WHERE id = ?")
            .bind(clip_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn enqueue_clip(&self, clip_id: Uuid) -> anyhow::Result<()> {
        let entry = QueueEntry {
            id: Uuid::new_v4(),
            clip_id,
            status: "pending".into(),
            attempts: 0,
            next_attempt_at: Utc::now(),
        };
        sqlx::query(
            "INSERT INTO queue (id, clip_id, status, attempts, next_attempt_at) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(entry.id.to_string())
        .bind(entry.clip_id.to_string())
        .bind(&entry.status)
        .bind(entry.attempts)
        .bind(entry.next_attempt_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_pending_queue(&self) -> anyhow::Result<Vec<QueueEntry>> {
        let rows = sqlx::query_as::<_, QueueRow>(
            "SELECT id, clip_id, status, attempts, next_attempt_at FROM queue WHERE status = 'pending' AND next_attempt_at <= ? ORDER BY next_attempt_at ASC"
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.try_into()).collect()
    }

    pub async fn update_queue_entry(&self, id: Uuid, status: &str, attempts: i32, next_attempt_at: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE queue SET status = ?, attempts = ?, next_attempt_at = ? WHERE id = ?"
        )
        .bind(status)
        .bind(attempts)
        .bind(next_attempt_at.to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_queue_entry(&self, clip_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM queue WHERE clip_id = ?")
            .bind(clip_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn save_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    #[allow(dead_code)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn insert_peer_device(&self, peer: &PeerDevice) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO peer_devices (id, name, public_key, ip, port, session_token, paired_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(peer.id.to_string())
        .bind(&peer.name)
        .bind(&peer.public_key)
        .bind(&peer.ip)
        .bind(peer.port as i64)
        .bind(&peer.session_token)
        .bind(peer.paired_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_peer_devices(&self) -> anyhow::Result<Vec<PeerDevice>> {
        let rows = sqlx::query_as::<_, PeerDeviceRow>(
            "SELECT id, name, public_key, ip, port, session_token, paired_at FROM peer_devices"
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into()).collect()
    }

    pub async fn delete_peer_device(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM peer_devices WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn update_peer_device_ip(&self, id: Uuid, ip: &str, port: u16) -> anyhow::Result<()> {
        sqlx::query("UPDATE peer_devices SET ip = ?, port = ? WHERE id = ?")
            .bind(ip)
            .bind(port as i64)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct ClipRow {
    id: String,
    user_id: String,
    device_id: String,
    content: String,
    format: String,
    e2e: i32,
    created_at: String,
    #[allow(dead_code)]
    pinned: i32,
    pin_slot: Option<i32>,
    category: Option<String>,
}

impl TryFrom<ClipRow> for Clip {
    type Error = anyhow::Error;
    fn try_from(r: ClipRow) -> anyhow::Result<Self> {
        Ok(Clip {
            id: r.id.parse()?,
            user_id: r.user_id.parse()?,
            device_id: r.device_id.parse()?,
            content: r.content,
            format: r.format,
            e2e: r.e2e != 0,
            created_at: r.created_at.parse()?,
            pin_slot: r.pin_slot.and_then(|v| if (1..=9).contains(&v) { Some(v as u8) } else { None }),
            category: r.category,
        })
    }
}

#[derive(sqlx::FromRow)]
struct QueueRow {
    id: String,
    clip_id: String,
    status: String,
    attempts: i32,
    next_attempt_at: String,
}

impl TryFrom<QueueRow> for QueueEntry {
    type Error = anyhow::Error;
    fn try_from(r: QueueRow) -> anyhow::Result<Self> {
        Ok(QueueEntry {
            id: r.id.parse()?,
            clip_id: r.clip_id.parse()?,
            status: r.status,
            attempts: r.attempts,
            next_attempt_at: r.next_attempt_at.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PeerDeviceRow {
    id: String,
    name: String,
    public_key: String,
    ip: String,
    port: i64,
    session_token: String,
    paired_at: String,
}

impl TryFrom<PeerDeviceRow> for PeerDevice {
    type Error = anyhow::Error;
    fn try_from(r: PeerDeviceRow) -> anyhow::Result<Self> {
        Ok(PeerDevice {
            id: r.id.parse()?,
            name: r.name,
            public_key: r.public_key,
            ip: r.ip,
            port: r.port as u16,
            session_token: r.session_token,
            paired_at: r.paired_at.parse()?,
            online: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_storage_crud() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::new(db_path.to_str().unwrap()).await.unwrap();

        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let clip = Clip {
            id: Uuid::new_v4(),
            user_id,
            device_id,
            content: "test content".into(),
            format: "text".into(),
            e2e: false,
            created_at: Utc::now(),
            pin_slot: None,
            category: None,
        };

        storage.insert_clip(&clip).await.unwrap();
        let clips = storage.get_recent_clips(user_id, 10).await.unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].content, "test content");

        storage.set_pin_slot(clip.id, Some(3)).await.unwrap();
        let clips = storage.get_recent_clips(user_id, 10).await.unwrap();
        assert_eq!(clips[0].pin_slot, Some(3));

        storage.clear_pin_slot(3).await.unwrap();
        let clips = storage.get_recent_clips(user_id, 10).await.unwrap();
        assert_eq!(clips[0].pin_slot, None);
    }

    #[tokio::test]
    async fn test_queue_operations() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::new(db_path.to_str().unwrap()).await.unwrap();

        let clip_id = Uuid::new_v4();
        storage.enqueue_clip(clip_id).await.unwrap();

        let pending = storage.get_pending_queue().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].clip_id, clip_id);

        storage.remove_queue_entry(clip_id).await.unwrap();
        let pending = storage.get_pending_queue().await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::new(db_path.to_str().unwrap()).await.unwrap();

        storage.save_setting("key1", "value1").await.unwrap();
        let val = storage.get_setting("key1").await.unwrap();
        assert_eq!(val, Some("value1".into()));
    }
}
