use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use uuid::Uuid;

use crate::models::{AuditLog, Clip, Device, User};

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await?;
        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                email TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL REFERENCES users(id),
                name TEXT NOT NULL,
                public_key TEXT,
                paired_at TEXT NOT NULL,
                revoked INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS clips (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                content TEXT NOT NULL,
                format TEXT NOT NULL DEFAULT 'text',
                e2e INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS audit_logs (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                device_id TEXT,
                action TEXT NOT NULL,
                details TEXT NOT NULL DEFAULT '{}',
                timestamp TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pairing_tokens (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                used INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS refresh_tokens (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                revoked INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_clips_user ON clips(user_id);
            CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_at);
            CREATE INDEX IF NOT EXISTS idx_devices_user ON devices(user_id);
            CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_logs(user_id);
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_refresh_device ON refresh_tokens(device_id);
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // Refresh tokens
    pub async fn create_refresh_token(
        &self,
        user_id: Uuid,
        device_id: Uuid,
        ttl_days: i64,
    ) -> anyhow::Result<(String, DateTime<Utc>)> {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let expires_at = Utc::now() + chrono::Duration::days(ttl_days);
        sqlx::query(
            "INSERT INTO refresh_tokens (token, user_id, device_id, expires_at, revoked) VALUES (?, ?, ?, ?, 0)"
        )
        .bind(&token)
        .bind(user_id.to_string())
        .bind(device_id.to_string())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok((token, expires_at))
    }

    /// Consume a refresh token: if valid, mark revoked and return (user_id, device_id).
    pub async fn consume_refresh_token(
        &self,
        token: &str,
    ) -> anyhow::Result<Option<(Uuid, Uuid)>> {
        let row = sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT user_id, device_id, expires_at, revoked FROM refresh_tokens WHERE token = ?",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;

        let Some((user_id, device_id, expires_at, revoked)) = row else {
            return Ok(None);
        };
        if revoked != 0 {
            return Ok(None);
        }
        let exp: DateTime<Utc> = expires_at.parse()?;
        if exp < Utc::now() {
            return Ok(None);
        }
        sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(Some((user_id.parse()?, device_id.parse()?)))
    }

    pub async fn revoke_refresh_tokens_for_device(&self, device_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE device_id = ?")
            .bind(device_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Users
    pub async fn create_user(&self, email: &str) -> anyhow::Result<User> {
        let user = User {
            id: Uuid::new_v4(),
            email: email.to_string(),
            created_at: Utc::now(),
        };
        sqlx::query("INSERT INTO users (id, email, created_at) VALUES (?, ?, ?)")
            .bind(user.id.to_string())
            .bind(&user.email)
            .bind(user.created_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(user)
    }

    #[allow(dead_code)]
    pub async fn get_user(&self, id: Uuid) -> anyhow::Result<Option<User>> {
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT id, email, created_at FROM users WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((id, email, created_at)) => Ok(Some(User {
                id: id.parse()?,
                email,
                created_at: created_at.parse()?,
            })),
            None => Ok(None),
        }
    }

    // Devices
    pub async fn create_device(
        &self,
        user_id: Uuid,
        name: &str,
        public_key: Option<&str>,
    ) -> anyhow::Result<Device> {
        let device = Device {
            id: Uuid::new_v4(),
            user_id,
            name: name.to_string(),
            public_key: public_key.map(String::from),
            paired_at: Utc::now(),
            revoked: false,
        };
        sqlx::query(
            "INSERT INTO devices (id, user_id, name, public_key, paired_at, revoked) VALUES (?, ?, ?, ?, ?, 0)",
        )
        .bind(device.id.to_string())
        .bind(device.user_id.to_string())
        .bind(&device.name)
        .bind(&device.public_key)
        .bind(device.paired_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(device)
    }

    pub async fn get_devices(&self, user_id: Uuid) -> anyhow::Result<Vec<Device>> {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>, String, i32)>(
            "SELECT id, user_id, name, public_key, paired_at, revoked FROM devices WHERE user_id = ?",
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, uid, name, pk, paired, rev)| {
                Ok(Device {
                    id: id.parse()?,
                    user_id: uid.parse()?,
                    name,
                    public_key: pk,
                    paired_at: paired.parse()?,
                    revoked: rev != 0,
                })
            })
            .collect()
    }

    pub async fn get_device(&self, id: Uuid) -> anyhow::Result<Option<Device>> {
        let row = sqlx::query_as::<_, (String, String, String, Option<String>, String, i32)>(
            "SELECT id, user_id, name, public_key, paired_at, revoked FROM devices WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((id, uid, name, pk, paired, rev)) => Ok(Some(Device {
                id: id.parse()?,
                user_id: uid.parse()?,
                name,
                public_key: pk,
                paired_at: paired.parse()?,
                revoked: rev != 0,
            })),
            None => Ok(None),
        }
    }

    pub async fn revoke_device(&self, device_id: Uuid) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ?")
            .bind(device_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // Clips
    pub async fn insert_clip(&self, clip: &Clip) -> anyhow::Result<bool> {
        let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clips WHERE id = ?")
            .bind(clip.id.to_string())
            .fetch_one(&self.pool)
            .await?;

        if existing > 0 {
            return Ok(false);
        }

        sqlx::query(
            "INSERT INTO clips (id, user_id, device_id, content, format, e2e, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(clip.id.to_string())
        .bind(clip.user_id.to_string())
        .bind(clip.device_id.to_string())
        .bind(&clip.content)
        .bind(&clip.format)
        .bind(clip.e2e as i32)
        .bind(clip.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    pub async fn get_clips(&self, user_id: Uuid, limit: i64) -> anyhow::Result<Vec<Clip>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, i32, String)>(
            "SELECT id, user_id, device_id, content, format, e2e, created_at FROM clips WHERE user_id = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(user_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, uid, did, content, format, e2e, created)| {
                Ok(Clip {
                    id: id.parse()?,
                    user_id: uid.parse()?,
                    device_id: did.parse()?,
                    content,
                    format,
                    e2e: e2e != 0,
                    created_at: created.parse()?,
                })
            })
            .collect()
    }

    // Audit logs
    pub async fn insert_audit_log(&self, log: &AuditLog) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO audit_logs (id, user_id, device_id, action, details, timestamp) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(log.id.to_string())
        .bind(log.user_id.to_string())
        .bind(log.device_id.map(|d| d.to_string()))
        .bind(&log.action)
        .bind(log.details.to_string())
        .bind(log.timestamp.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_audit_logs(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<AuditLog>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, String, String, String)>(
            "SELECT id, user_id, device_id, action, details, timestamp FROM audit_logs WHERE user_id = ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(user_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, uid, did, action, details, ts)| {
                Ok(AuditLog {
                    id: id.parse()?,
                    user_id: uid.parse()?,
                    device_id: did.map(|d| d.parse()).transpose()?,
                    action,
                    details: serde_json::from_str(&details)?,
                    timestamp: ts.parse()?,
                })
            })
            .collect()
    }

    // Pairing tokens
    pub async fn create_pairing_token(
        &self,
        user_id: Uuid,
        token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO pairing_tokens (token, user_id, expires_at, used) VALUES (?, ?, ?, 0)",
        )
        .bind(token)
        .bind(user_id.to_string())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn consume_pairing_token(&self, token: &str) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, (String, String, i32)>(
            "SELECT user_id, expires_at, used FROM pairing_tokens WHERE token = ?",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((user_id, expires_at, used)) => {
                if used != 0 {
                    return Ok(None);
                }
                let exp: DateTime<Utc> = expires_at.parse()?;
                if exp < Utc::now() {
                    return Ok(None);
                }
                sqlx::query("UPDATE pairing_tokens SET used = 1 WHERE token = ?")
                    .bind(token)
                    .execute(&self.pool)
                    .await?;
                Ok(Some(user_id.parse()?))
            }
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Database {
        Database::new("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn test_user_crud() {
        let db = test_db().await;
        let user = db.create_user("test@example.com").await.unwrap();
        let fetched = db.get_user(user.id).await.unwrap().unwrap();
        assert_eq!(fetched.email, "test@example.com");
    }

    #[tokio::test]
    async fn test_device_crud() {
        let db = test_db().await;
        let user = db.create_user("test@example.com").await.unwrap();
        let device = db
            .create_device(user.id, "My PC", Some("pubkey123"))
            .await
            .unwrap();

        let devices = db.get_devices(user.id).await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "My PC");

        db.revoke_device(device.id).await.unwrap();
        let dev = db.get_device(device.id).await.unwrap().unwrap();
        assert!(dev.revoked);
    }

    #[tokio::test]
    async fn test_clip_idempotency() {
        let db = test_db().await;
        let user = db.create_user("test@example.com").await.unwrap();

        let clip = Clip {
            id: Uuid::new_v4(),
            user_id: user.id,
            device_id: Uuid::new_v4(),
            content: "test".into(),
            format: "text".into(),
            e2e: false,
            created_at: Utc::now(),
        };

        assert!(db.insert_clip(&clip).await.unwrap());
        assert!(!db.insert_clip(&clip).await.unwrap());
    }

    #[tokio::test]
    async fn test_audit_logs() {
        let db = test_db().await;
        let user = db.create_user("test@example.com").await.unwrap();

        let log = AuditLog {
            id: Uuid::new_v4(),
            user_id: user.id,
            device_id: None,
            action: "clip_upload".into(),
            details: serde_json::json!({"clip_id": "test"}),
            timestamp: Utc::now(),
        };
        db.insert_audit_log(&log).await.unwrap();

        let logs = db.get_audit_logs(user.id, 10).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].action, "clip_upload");
    }

    #[tokio::test]
    async fn test_pairing_tokens() {
        let db = test_db().await;
        let user = db.create_user("test@example.com").await.unwrap();
        let token = "test-token-123";
        let expires = Utc::now() + chrono::Duration::hours(1);

        db.create_pairing_token(user.id, token, expires)
            .await
            .unwrap();

        let uid = db.consume_pairing_token(token).await.unwrap().unwrap();
        assert_eq!(uid, user.id);

        // Second consumption should fail
        assert!(db.consume_pairing_token(token).await.unwrap().is_none());
    }
}
