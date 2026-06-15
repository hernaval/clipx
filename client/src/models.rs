use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub content: Option<String>,
    pub format: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub source_app: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub content: String,
    pub format: String,
    pub e2e: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub pin_slot: Option<u8>,
}

impl Clip {
    pub fn is_pinned(&self) -> bool {
        self.pin_slot.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipUpload {
    pub device_id: Uuid,
    pub clip_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub format: String,
    pub content: String,
    pub e2e: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub public_key: Option<String>,
    pub paired_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: Uuid,
    pub clip_id: Uuid,
    pub status: String,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: Option<i64>,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<i64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub server_url: String,
    pub device_id: Uuid,
    pub device_name: String,
    pub user_id: Uuid,
    pub jwt: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub jwt_expires_at: Option<DateTime<Utc>>,
    pub overlay_hotkey: String,
    pub e2e_enabled: bool,
    pub local_sync_port: u16,
    #[serde(default = "default_true")]
    pub lan_enabled: bool,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub private_key: String,
    #[serde(default = "default_cloud_retries")]
    pub max_cloud_retries: u32,
    #[serde(default = "default_true")]
    pub auto_paste_inbound: bool,
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    #[serde(default)]
    pub excluded_apps: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:8443".into(),
            device_id: Uuid::new_v4(),
            device_name: hostname(),
            user_id: Uuid::new_v4(),
            jwt: None,
            refresh_token: None,
            jwt_expires_at: None,
            overlay_hotkey: "Ctrl+Shift+V".into(),
            e2e_enabled: false,
            local_sync_port: 41234,
            lan_enabled: true,
            public_key: String::new(),
            private_key: String::new(),
            max_cloud_retries: 10,
            auto_paste_inbound: true,
            history_limit: 50,
            excluded_apps: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BootstrapRequest {
    pub email: String,
    pub device_name: String,
}

#[derive(Debug, Deserialize)]
pub struct BootstrapResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub jwt: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub jwt_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Deserialize)]
pub struct RefreshResponse {
    pub jwt: String,
    pub refresh_token: String,
    pub jwt_expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PairConfirmRequest {
    pub token: String,
    pub device_name: String,
}

#[derive(Debug, Deserialize)]
pub struct PairConfirmResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub jwt: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub jwt_expires_at: Option<DateTime<Utc>>,
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn default_true() -> bool { true }
fn default_cloud_retries() -> u32 { 10 }
fn default_history_limit() -> u32 { 50 }

// ── LAN peer-to-peer models ───────────────────────────────────────────────────

/// Generates a cryptographically random 32-byte hex session token for peer auth.
pub fn generate_session_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A trusted LAN peer stored in the local `peer_devices` SQLite table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDevice {
    pub id: uuid::Uuid,
    pub name: String,
    pub public_key: String,
    pub ip: String,
    pub port: u16,
    pub session_token: String,
    pub paired_at: DateTime<Utc>,
    #[serde(default)]
    pub online: bool,
}

/// Body of `POST /pair/request` (Device A → Device B's embedded server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanPairRequest {
    pub from_device_id: uuid::Uuid,
    pub from_name: String,
    pub from_public_key: String,
    pub ip: String,
    pub port: u16,
    pub session_token: String,
}

/// Body of `POST /pair/accept` (Device B → Device A's embedded server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanPairResponse {
    pub accepted: bool,
    pub to_device_id: uuid::Uuid,
    pub to_name: String,
    pub to_public_key: String,
    pub ip: String,
    pub port: u16,
    pub session_token: String,
}

/// Sent over the UI channel when a pairing request arrives from a peer.
#[derive(Debug, Clone)]
pub struct InboundPairRequest {
    pub request: LanPairRequest,
    pub respond_url: String,
}
