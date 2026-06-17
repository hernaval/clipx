use std::time::Duration;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::models::{Clip, ClipUpload, Device, InboundPairRequest, LanPairRequest, LanPairResponse, PeerDevice, RefreshRequest, RefreshResponse, Settings, WsMessage};
use crate::state::{LogLevel, SharedStateHandle};
use crate::storage::Storage;

pub struct SyncEngine {
    storage: std::sync::Arc<Storage>,
    settings: std::sync::Arc<tokio::sync::RwLock<Settings>>,
    shared: SharedStateHandle,
}

impl SyncEngine {
    pub fn new(
        storage: std::sync::Arc<Storage>,
        settings: std::sync::Arc<tokio::sync::RwLock<Settings>>,
        shared: SharedStateHandle,
    ) -> Self {
        Self { storage, settings, shared }
    }

    /// Exchange the stored refresh token for a fresh JWT.
    /// On success, settings.jwt / refresh_token / jwt_expires_at are updated
    /// in memory AND persisted to SQLite. Returns Ok(()) only if everything
    /// succeeded; on auth failure the caller should give up and clear settings.jwt.
    async fn try_refresh_jwt(&self) -> anyhow::Result<()> {
        let (server_url, refresh) = {
            let s = self.settings.read().await;
            (s.server_url.clone(), s.refresh_token.clone())
        };
        let Some(refresh) = refresh else {
            anyhow::bail!("no refresh token available");
        };

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{server_url}/api/v1/auth/refresh"))
            .json(&RefreshRequest { refresh_token: refresh })
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("refresh rejected: HTTP {}", resp.status());
        }
        let body: RefreshResponse = resp.json().await?;

        let mut s = self.settings.write().await;
        s.jwt = Some(body.jwt);
        s.refresh_token = Some(body.refresh_token);
        s.jwt_expires_at = Some(body.jwt_expires_at);
        let json = serde_json::to_string(&*s).unwrap_or_default();
        drop(s);
        let _ = self.storage.save_setting("config", &json).await;
        self.shared.lock().unwrap().log(LogLevel::Info, "JWT refreshed.");
        Ok(())
    }

    /// Background task: every minute, if the JWT will expire within 10 minutes,
    /// proactively refresh it. Exits gracefully if no refresh token is present.
    pub async fn run_jwt_refresh_loop(self: std::sync::Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let (have_refresh, exp) = {
                let s = self.settings.read().await;
                (s.refresh_token.is_some(), s.jwt_expires_at)
            };
            if !have_refresh {
                continue;
            }
            let now = chrono::Utc::now();
            let needs = match exp {
                Some(e) => (e - now).num_seconds() < 600,
                None => false, // unknown expiry — leave alone; reactive path handles 401
            };
            if needs {
                if let Err(e) = self.try_refresh_jwt().await {
                    warn!("Proactive JWT refresh failed: {e}");
                }
            }
        }
    }

    /// Clear the JWT in settings + persist. Caller should also stop reusing the
    /// in-memory JWT for the rest of the operation.
    async fn clear_jwt(&self) {
        let mut s = self.settings.write().await;
        s.jwt = None;
        s.refresh_token = None;
        s.jwt_expires_at = None;
        let json = serde_json::to_string(&*s).unwrap_or_default();
        drop(s);
        let _ = self.storage.save_setting("config", &json).await;
    }

    /// Refresh the cached device list from the server if E2E is on and the cache
    /// is empty or older than 5 minutes. Failure is non-fatal — `upload_clip` will
    /// fall back to whatever's already cached.
    async fn refresh_devices_if_stale(&self, server_url: &str, jwt: &str) {
        const TTL: Duration = Duration::from_secs(5 * 60);
        let needs_refresh = {
            let s = self.shared.lock().unwrap();
            s.devices.is_empty() || s.devices_fetched_at.map(|t| t.elapsed() > TTL).unwrap_or(true)
        };
        if !needs_refresh {
            return;
        }

        let client = reqwest::Client::new();
        let resp = match client
            .get(format!("{server_url}/api/v1/devices"))
            .bearer_auth(jwt)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!("Device cache refresh skipped: {e}");
                return;
            }
        };
        if !resp.status().is_success() {
            debug!("Device cache refresh got HTTP {}", resp.status());
            return;
        }
        match resp.json::<Vec<Device>>().await {
            Ok(devices) => {
                let mut s = self.shared.lock().unwrap();
                s.devices = devices;
                s.devices_fetched_at = Some(std::time::Instant::now());
            }
            Err(e) => debug!("Device cache parse failed: {e}"),
        }
    }

    pub async fn upload_clip(&self, clip: &Clip) -> anyhow::Result<()> {
        let (jwt_owned, e2e_on, server_url) = {
            let settings = self.settings.read().await;
            let Some(jwt) = settings.jwt.clone() else { return Ok(()); };
            (jwt, settings.e2e_enabled, settings.server_url.clone())
        };

        if e2e_on {
            self.refresh_devices_if_stale(&server_url, &jwt_owned).await;
        }

        let settings = self.settings.read().await;
        let jwt = jwt_owned.as_str();

        // If E2E is on, encrypt content for all known cloud peers.
        let (content, e2e_flag) = if e2e_on {
            let recipients: Vec<(String, String)> = self
                .shared
                .lock()
                .unwrap()
                .devices
                .iter()
                .filter(|d| !d.revoked)
                .filter_map(|d| {
                    d.public_key.as_ref().filter(|k| !k.is_empty())
                        .map(|k| (d.id.to_string(), k.clone()))
                })
                .collect();
            if recipients.is_empty() {
                warn!("E2E enabled but no recipient pubkeys cached; uploading plaintext");
                (clip.content.clone(), clip.e2e)
            } else {
                match crate::crypto::encrypt_for_recipients(&clip.content, &recipients) {
                    Ok(enc) => match crate::crypto::pack(&enc) {
                        Ok(s) => (s, true),
                        Err(_) => (clip.content.clone(), clip.e2e),
                    },
                    Err(e) => {
                        warn!("E2E encrypt failed, sending plaintext: {e}");
                        (clip.content.clone(), clip.e2e)
                    }
                }
            }
        } else {
            (clip.content.clone(), clip.e2e)
        };

        let upload = ClipUpload {
            device_id: clip.device_id,
            clip_id: clip.id,
            timestamp: clip.created_at,
            format: clip.format.clone(),
            content,
            e2e: e2e_flag,
        };

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/api/v1/clips", settings.server_url))
            .bearer_auth(jwt)
            .json(&upload)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        match resp.status().as_u16() {
            201 => {
                info!(clip_id = %clip.id, "Clip uploaded successfully");
                self.storage.remove_queue_entry(clip.id).await?;
                let preview = clip.content.chars().take(50).collect::<String>();
                let mut s = self.shared.lock().unwrap();
                s.upload_count += 1;
                s.log(LogLevel::Success, format!("Uploaded: \"{}\"", preview));
                Ok(())
            }
            409 => {
                info!(clip_id = %clip.id, "Clip already exists (duplicate)");
                self.storage.remove_queue_entry(clip.id).await?;
                self.shared.lock().unwrap().log(LogLevel::Info, "Skipped duplicate clip");
                Ok(())
            }
            401 => {
                warn!("Unauthorized on upload — attempting refresh");
                // Try refresh once; on success, bail with retriable error so the queue retries.
                if self.try_refresh_jwt().await.is_ok() {
                    anyhow::bail!("Unauthorized (refreshed, retrying)");
                }
                // Refresh failed too — give up.
                self.clear_jwt().await;
                self.shared.lock().unwrap().log(
                    LogLevel::Error,
                    "Cloud auth expired — please re-pair this device from the Setup screen.",
                );
                anyhow::bail!("Unauthorized")
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                self.shared.lock().unwrap().log(LogLevel::Error, format!("Upload failed: HTTP {status}"));
                anyhow::bail!("Upload failed with status {status}: {body}")
            }
        }
    }

    pub async fn process_queue(&self) -> anyhow::Result<()> {
        let pending = self.storage.get_pending_queue().await?;
        if pending.is_empty() {
            return Ok(());
        }

        debug!(count = pending.len(), "Processing upload queue");

        for entry in pending {
            let settings = self.settings.read().await;
            let user_id = settings.user_id;
            drop(settings);

            let clips = self.storage.get_recent_clips(user_id, 1000).await?;
            let clip = clips.iter().find(|c| c.id == entry.clip_id);

            if let Some(clip) = clip {
                match self.upload_clip(clip).await {
                    Ok(()) => {}
                    Err(_e) => {
                        let new_attempts = entry.attempts + 1;
                        if new_attempts >= 10 {
                            warn!(clip_id = %entry.clip_id, "Max retries reached, marking failed");
                            self.storage
                                .update_queue_entry(entry.id, "failed", new_attempts, Utc::now())
                                .await?;
                        } else {
                            let backoff = Duration::from_secs(2u64.pow(new_attempts as u32).min(300));
                            let next = Utc::now() + backoff;
                            debug!(clip_id = %entry.clip_id, attempts = new_attempts, "Retrying after backoff");
                            self.storage
                                .update_queue_entry(entry.id, "pending", new_attempts, next)
                                .await?;
                        }
                    }
                }
            } else {
                self.storage.remove_queue_entry(entry.clip_id).await?;
            }
        }
        Ok(())
    }

    pub async fn run_queue_processor(self: std::sync::Arc<Self>) {
        loop {
            if let Err(e) = self.process_queue().await {
                warn!("Queue processing error: {e}");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Runs the cloud WebSocket listener with bounded retries.
    /// Returns Ok(()) when the retry limit is hit — caller should restart after a backoff.
    pub async fn run_ws_listener(
        &self,
        clip_tx: mpsc::Sender<Clip>,
    ) -> anyhow::Result<()> {
        let max_retries = self.settings.read().await.max_cloud_retries;
        let mut consecutive_failures = 0u32;

        loop {
            let had_jwt = self.settings.read().await.jwt.is_some();

            match self.ws_connect(&clip_tx).await {
                Ok(()) if !had_jwt => {
                    // No JWT — ws_connect slept 30 s and returned; not a failure.
                    self.shared.lock().unwrap().ws_connected = false;
                }
                Ok(()) => {
                    // Clean disconnect (was connected, then closed).
                    // Reset the failure counter — the server is reachable.
                    consecutive_failures = 0;
                    self.shared.lock().unwrap().ws_connected = false;
                    info!("Cloud WebSocket closed cleanly, reconnecting…");
                }
                Err(e) if !had_jwt => {
                    // Shouldn't happen (ws_connect returns Ok when no JWT), but guard anyway.
                    self.shared.lock().unwrap().ws_connected = false;
                    warn!("Unexpected WS error with no JWT: {e}");
                }
                Err(e) => {
                    consecutive_failures += 1;
                    {
                        let mut s = self.shared.lock().unwrap();
                        s.ws_connected = false;
                        s.log(LogLevel::Warn,
                            format!("Cloud sync error ({consecutive_failures}/{max_retries}): {e}"));
                    }
                    warn!("Cloud WS error ({consecutive_failures}/{max_retries}): {e}");

                    if consecutive_failures >= max_retries {
                        let msg = format!(
                            "Cloud sync stopped after {max_retries} failed attempts. Will retry in 5 minutes."
                        );
                        warn!("{msg}");
                        self.shared.lock().unwrap().log(LogLevel::Error, msg);
                        self.shared.lock().unwrap().cloud_sync_stopped = true;
                        // Signal caller to restart after a longer backoff.
                        return Ok(());
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn ws_connect(&self, clip_tx: &mpsc::Sender<Clip>) -> anyhow::Result<()> {
        let settings = self.settings.read().await;
        if settings.jwt.is_none() {
            drop(settings);
            tokio::time::sleep(Duration::from_secs(30)).await;
            return Ok(());
        }
        let jwt = settings.jwt.as_deref().unwrap_or_default();
        let ws_url = settings
            .server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let url = format!("{ws_url}/api/v1/ws?token={jwt}");
        drop(settings);

        let endpoint = url.split('?').next().unwrap_or(&url).to_string();
        info!("Connecting to WebSocket: {endpoint}");
        self.shared.lock().unwrap().log(LogLevel::Info, format!("Connecting to {endpoint}"));

        let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
            Ok(pair) => pair,
            Err(tokio_tungstenite::tungstenite::Error::Http(resp))
                if resp.status() == 401 || resp.status() == 403 =>
            {
                warn!("Cloud WS auth rejected (status {}); trying refresh", resp.status());
                if self.try_refresh_jwt().await.is_ok() {
                    // Reconnect with the new JWT on the next iteration.
                    anyhow::bail!("Unauthorized (refreshed)")
                }
                self.clear_jwt().await;
                self.shared.lock().unwrap().log(
                    LogLevel::Error,
                    "Cloud auth expired — please re-pair this device from the Setup screen.",
                );
                anyhow::bail!("Unauthorized")
            }
            Err(e) => return Err(e.into()),
        };
        let (mut write, mut read) = ws.split();

        {
            let mut s = self.shared.lock().unwrap();
            s.ws_connected = true;
            s.log(LogLevel::Success, "WebSocket connected to sync server");
        }

        let ping_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if write.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
        });

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(ws_msg) = serde_json::from_str::<WsMessage>(&text) {
                        if ws_msg.msg_type == "clip" {
                            if let Ok(mut clip) = serde_json::from_value::<Clip>(ws_msg.payload) {
                                // Decrypt if E2E.
                                let (my_id, my_priv) = {
                                    let s = self.settings.read().await;
                                    (s.device_id.to_string(), s.private_key.clone())
                                };
                                match crate::crypto::try_decrypt_in_place(&mut clip.content, &my_id, &my_priv) {
                                    Ok(true) => { clip.e2e = true; }
                                    Ok(false) => {}
                                    Err(e) => {
                                        warn!("Cloud E2E decrypt failed for {}: {e}", clip.id);
                                        continue;
                                    }
                                }
                                info!(clip_id = %clip.id, "Received clip via WebSocket");
                                let preview = clip.content.chars().take(50).collect::<String>();
                                {
                                    let mut s = self.shared.lock().unwrap();
                                    s.receive_count += 1;
                                    s.log(LogLevel::Info, format!("Received clip: \"{}\"", preview));
                                    s.add_clip(clip.clone());
                                }
                                let _ = clip_tx.send(clip).await;
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    error!("WebSocket error: {e}");
                    break;
                }
                _ => {}
            }
        }

        ping_handle.abort();
        Ok(())
    }
}

// ── LAN embedded server ───────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LanState {
    pub storage: std::sync::Arc<Storage>,
    pub shared: SharedStateHandle,
    pub pair_tx: mpsc::Sender<InboundPairRequest>,
    pub settings: std::sync::Arc<RwLock<Settings>>,
}

pub struct LocalSyncServer {
    state: LanState,
    port: u16,
}

impl LocalSyncServer {
    pub fn new(state: LanState, port: u16) -> Self {
        Self { state, port }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        use axum::{routing::{get, post}, Router};

        let state = self.state;
        let app = Router::new()
            .route("/ws",           get(handle_lan_ws))
            .route("/pair/request", post(handle_pair_request))
            .route("/pair/accept",  post(handle_pair_accept))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.port));
        info!("LAN sync server listening on {addr}");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct WsQuery {
    token: Option<String>,
}

async fn handle_lan_ws(
    ws: axum::extract::WebSocketUpgrade,
    axum::extract::Query(params): axum::extract::Query<WsQuery>,
    axum::extract::State(state): axum::extract::State<LanState>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let peer = {
        let s = state.shared.lock().unwrap();
        params.token.as_deref().and_then(|t| {
            s.peer_devices.iter().find(|p| p.session_token == t).cloned()
        })
    };

    let peer = match peer {
        Some(p) => p,
        None => {
            warn!("LAN WS rejected: invalid or missing token");
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    };

    ws.on_upgrade(move |socket| lan_ws_socket(socket, peer, state))
        .into_response()
}

async fn lan_ws_socket(mut socket: axum::extract::ws::WebSocket, peer: PeerDevice, state: LanState) {
    use axum::extract::ws::Message as AxMsg;

    info!("LAN peer {} connected", peer.name);
    state.shared.lock().unwrap().set_peer_online(peer.id, true);

    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            AxMsg::Text(text) => {
                if let Ok(ws_msg) = serde_json::from_str::<WsMessage>(&text) {
                    if ws_msg.msg_type == "clip" {
                        if let Ok(mut clip) = serde_json::from_value::<Clip>(ws_msg.payload) {
                            let (my_id, my_priv) = {
                                let s = state.settings.read().await;
                                (s.device_id.to_string(), s.private_key.clone())
                            };
                            match crate::crypto::try_decrypt_in_place(&mut clip.content, &my_id, &my_priv) {
                                Ok(true) => { clip.e2e = true; }
                                Ok(false) => {}
                                Err(e) => {
                                    warn!("LAN E2E decrypt failed for {}: {e}", clip.id);
                                    continue;
                                }
                            }
                            let preview = clip.content.chars().take(50).collect::<String>();
                            {
                                let mut s = state.shared.lock().unwrap();
                                s.receive_count += 1;
                                s.add_clip(clip.clone());
                                s.log(LogLevel::Info, format!("LAN clip from {}: \"{}\"", peer.name, preview));
                            }
                            if let Err(e) = state.storage.insert_clip(&clip).await {
                                error!("Failed to store LAN clip: {e}");
                            }
                            let should_paste = state.settings.read().await.auto_paste_inbound;
                            if should_paste {
                                let content = clip.content.clone();
                                let shared = state.shared.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = crate::ipc::IpcClient::send_rpc(
                                        "set_clipboard",
                                        Some(serde_json::json!({ "content": content })),
                                    ).await {
                                        shared.lock().unwrap().log(
                                            LogLevel::Warn,
                                            format!("Auto-paste skipped — bridge unreachable: {e}"),
                                        );
                                    }
                                });
                            }
                        }
                    }
                }
            }
            AxMsg::Ping(data) => { let _ = socket.send(AxMsg::Pong(data)).await; }
            AxMsg::Close(_) => break,
            _ => {}
        }
    }

    info!("LAN peer {} disconnected", peer.name);
    state.shared.lock().unwrap().set_peer_online(peer.id, false);
}

async fn handle_pair_request(
    axum::extract::State(state): axum::extract::State<LanState>,
    axum::Json(req): axum::Json<LanPairRequest>,
) -> axum::http::StatusCode {
    let already_paired = {
        let s = state.shared.lock().unwrap();
        s.peer_devices.iter().any(|p| p.id == req.from_device_id)
    };
    if already_paired {
        return axum::http::StatusCode::CONFLICT;
    }

    let respond_url = format!("http://{}:{}/pair/accept", req.ip, req.port);
    let inbound = InboundPairRequest { request: req, respond_url };

    match state.pair_tx.send(inbound).await {
        Ok(()) => axum::http::StatusCode::ACCEPTED,
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn handle_pair_accept(
    axum::extract::State(state): axum::extract::State<LanState>,
    axum::Json(resp): axum::Json<LanPairResponse>,
) -> axum::http::StatusCode {
    if !resp.accepted {
        return axum::http::StatusCode::OK;
    }

    let peer = PeerDevice {
        id: resp.to_device_id,
        name: resp.to_name,
        public_key: resp.to_public_key,
        ip: resp.ip,
        port: resp.port,
        session_token: resp.session_token,
        paired_at: Utc::now(),
        online: false,
    };

    if let Err(e) = state.storage.insert_peer_device(&peer).await {
        error!("Failed to store LAN peer from accept: {e}");
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
    }

    let name = peer.name.clone();
    state.shared.lock().unwrap().peer_devices.push(peer);
    info!("LAN peer {} accepted and stored", name);

    axum::http::StatusCode::OK
}
