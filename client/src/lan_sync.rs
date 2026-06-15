use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use futures_util::SinkExt;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::models::{Clip, Settings};
use crate::state::{LogLevel, SharedStateHandle};
use crate::storage::Storage;

pub struct LanPeerManager {
    storage: Arc<Storage>,
    shared: SharedStateHandle,
    settings: Arc<tokio::sync::RwLock<Settings>>,
    clip_broadcast: broadcast::Sender<Clip>,
    active: Arc<Mutex<HashSet<Uuid>>>,
}

impl LanPeerManager {
    pub fn new(
        storage: Arc<Storage>,
        shared: SharedStateHandle,
        settings: Arc<tokio::sync::RwLock<Settings>>,
        clip_broadcast: broadcast::Sender<Clip>,
    ) -> Self {
        Self {
            storage,
            shared,
            settings,
            clip_broadcast,
            active: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Main loop: polls for trusted peers every 2 s and spawns a connection task
    /// for any peer that doesn't have one yet.
    pub async fn run(&self) {
        loop {
            let peers = self.shared.lock().unwrap().peer_devices.clone();

            for peer in peers {
                let already_active = self.active.lock().unwrap().contains(&peer.id);
                if !already_active {
                    self.active.lock().unwrap().insert(peer.id);

                    let active_handle = self.active.clone();
                    let shared = self.shared.clone();
                    let storage = self.storage.clone();
                    let settings = self.settings.clone();
                    let mut broadcast_rx = self.clip_broadcast.subscribe();

                    tokio::spawn(async move {
                        run_peer_connection(peer.clone(), &mut broadcast_rx, shared.clone(), storage.clone(), settings.clone()).await;
                        // Mark inactive so the manager can reconnect later
                        active_handle.lock().unwrap().remove(&peer.id);
                        shared.lock().unwrap().set_peer_online(peer.id, false);
                    });
                }
            }

            // Clean up entries for peers that were removed (forgotten)
            {
                let current_ids: HashSet<Uuid> = self
                    .shared
                    .lock()
                    .unwrap()
                    .peer_devices
                    .iter()
                    .map(|p| p.id)
                    .collect();
                self.active.lock().unwrap().retain(|id| current_ids.contains(id));
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Persistent outbound WS connection to one peer. Sends local clips to that peer.
/// Reconnects with 5 s backoff on disconnect.
async fn run_peer_connection(
    peer: crate::models::PeerDevice,
    broadcast_rx: &mut broadcast::Receiver<Clip>,
    shared: SharedStateHandle,
    _storage: Arc<Storage>,
    settings: Arc<tokio::sync::RwLock<Settings>>,
) {
    loop {
        // Check if peer is still trusted
        let still_trusted = shared.lock().unwrap().peer_devices.iter().any(|p| p.id == peer.id);
        if !still_trusted {
            info!("Peer {} removed, stopping connection task", peer.name);
            return;
        }

        let url = format!("ws://{}:{}/ws?token={}", peer.ip, peer.port, peer.session_token);
        info!("Connecting to LAN peer {} at {}", peer.name, url);

        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => {
                info!("Connected to LAN peer {}", peer.name);
                shared.lock().unwrap().set_peer_online(peer.id, true);
                shared.lock().unwrap().log(LogLevel::Success, format!("LAN peer {} connected", peer.name));

                let (mut write, _read) = futures_util::StreamExt::split(ws);

                // Send clips from broadcast until error or peer removed
                loop {
                    // Check trust before each send
                    let still_trusted = shared.lock().unwrap().peer_devices.iter().any(|p| p.id == peer.id);
                    if !still_trusted {
                        return;
                    }

                    match broadcast_rx.recv().await {
                        Ok(clip) => {
                            // Don't forward clips that originated from this peer
                            if clip.device_id == peer.id {
                                continue;
                            }

                            // E2E: encrypt for this specific peer if enabled.
                            let s = settings.read().await;
                            let e2e_on = s.e2e_enabled;
                            drop(s);

                            let mut out = clip.clone();
                            if e2e_on && !peer.public_key.is_empty() {
                                match crate::crypto::encrypt_for_recipients(
                                    &clip.content,
                                    &[(peer.id.to_string(), peer.public_key.clone())],
                                ) {
                                    Ok(enc) => {
                                        if let Ok(packed) = crate::crypto::pack(&enc) {
                                            out.content = packed;
                                            out.e2e = true;
                                        }
                                    }
                                    Err(e) => {
                                        warn!("LAN E2E encrypt for {} failed: {e}", peer.name);
                                    }
                                }
                            }

                            let mut val = match serde_json::to_value(&out) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            val["type"] = serde_json::Value::String("clip".into());
                            let text = val.to_string();

                            if write.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("LAN broadcast lagged by {n} for peer {}", peer.name);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }

                shared.lock().unwrap().set_peer_online(peer.id, false);
                shared.lock().unwrap().log(LogLevel::Warn, format!("LAN peer {} disconnected", peer.name));
            }
            Err(e) => {
                warn!("Cannot reach LAN peer {}: {e}", peer.name);
            }
        }

        // Check trust before reconnecting
        let still_trusted = shared.lock().unwrap().peer_devices.iter().any(|p| p.id == peer.id);
        if !still_trusted {
            return;
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
