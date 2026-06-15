mod crypto;
mod hotkeys;
mod ipc;
mod lan_sync;
mod models;
mod overlay;
mod state;
mod storage;
mod sync;
mod tray;
mod ui;

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};
use uuid::Uuid;

use crate::lan_sync::LanPeerManager;
use crate::models::{Clip, ClipboardEvent, InboundPairRequest, Settings};
use crate::state::{LogLevel, SharedState, SharedStateHandle};
use crate::storage::Storage;
use crate::sync::{LanState, LocalSyncServer, SyncEngine};
use crate::ui::ClipXApp;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "clipx_client=info".into()),
        )
        .init();

    info!("ClipX client starting...");

    let data_dir = directories::ProjectDirs::from("com", "clipx", "ClipX")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&data_dir)?;
    let db_path = data_dir.join("clipx.db");

    // Shared UI state
    let shared: SharedStateHandle = Arc::new(std::sync::Mutex::new(SharedState::new()));

    // Build tokio runtime for background async tasks
    let rt = tokio::runtime::Runtime::new()?;

    // Initialize storage and settings synchronously
    let storage = rt.block_on(async {
        Storage::new(db_path.to_str().unwrap()).await
    })?;
    let storage = Arc::new(storage);

    let settings = rt.block_on(async {
        load_or_create_settings(&storage).await
    })?;
    let settings = Arc::new(RwLock::new(settings));

    let sync_engine = Arc::new(SyncEngine::new(
        storage.clone(),
        settings.clone(),
        shared.clone(),
    ));

    // Populate history from DB on startup, honoring the user's history_limit.
    {
        let (user_id, history_limit) = rt.block_on(async {
            let s = settings.read().await;
            (s.user_id, s.history_limit as i64)
        });
        let recent = rt.block_on(async {
            storage.get_recent_clips(user_id, history_limit).await
        })?;
        let count = recent.len();
        let mut s = shared.lock().unwrap();
        s.history_limit = history_limit as usize;
        for clip in recent.into_iter().rev() {
            s.add_clip(clip);
        }
        s.log(LogLevel::Info, format!("Loaded {count} clips from history"));
    }

    // Ensure device has a valid X25519 keypair (regenerate if missing or wrong length)
    rt.block_on(async {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let s = settings.read().await;
        let pub_ok = engine.decode(&s.public_key).map(|v| v.len() == 32).unwrap_or(false);
        let priv_ok = engine.decode(&s.private_key).map(|v| v.len() == 32).unwrap_or(false);
        let valid = !s.public_key.is_empty() && !s.private_key.is_empty() && pub_ok && priv_ok;
        drop(s);
        if !valid {
            if let Ok(kp) = crate::crypto::generate_keypair() {
                let mut s = settings.write().await;
                s.public_key = kp.public_key;
                s.private_key = kp.private_key;
                let json = serde_json::to_string(&*s).unwrap_or_default();
                drop(s);
                let _ = storage.save_setting("config", &json).await;
            }
        }
    });

    // Load trusted LAN peers from DB
    {
        let peers = rt.block_on(async { storage.get_peer_devices().await })?;
        let count = peers.len();
        shared.lock().unwrap().peer_devices = peers;
        if count > 0 {
            shared.lock().unwrap().log(LogLevel::Info, format!("Loaded {count} LAN peer(s)"));
        }
    }

    // LAN clip broadcast channel (local clips → peer connections)
    let (lan_clip_tx, _) = tokio::sync::broadcast::channel::<Clip>(100);

    // Inbound pairing request channel (async server → sync UI)
    let (pair_inbound_tx, mut pair_inbound_rx) = mpsc::channel::<InboundPairRequest>(4);
    let (pair_ui_tx, pair_ui_rx) = std::sync::mpsc::sync_channel::<InboundPairRequest>(1);
    rt.spawn(async move {
        while let Some(req) = pair_inbound_rx.recv().await {
            let _ = pair_ui_tx.try_send(req);
        }
    });

    // Spawn LAN embedded server
    {
        let lan_port = rt.block_on(async { settings.read().await.local_sync_port });
        let lan_state = LanState {
            storage: storage.clone(),
            shared: shared.clone(),
            pair_tx: pair_inbound_tx,
            settings: settings.clone(),
        };
        let server = LocalSyncServer::new(lan_state, lan_port);
        shared.lock().unwrap().lan_server_running = true;
        rt.spawn(async move {
            if let Err(e) = server.run().await {
                tracing::error!("LAN server error: {e}");
            }
        });
    }

    // Spawn LAN peer manager (outbound WS connections)
    {
        let mgr = std::sync::Arc::new(LanPeerManager::new(
            storage.clone(),
            shared.clone(),
            settings.clone(),
            lan_clip_tx.clone(),
        ));
        rt.spawn(async move { mgr.run().await });
    }

    // Channels
    let (clip_event_tx, mut clip_event_rx) = mpsc::channel::<ClipboardEvent>(100);
    let (inbound_clip_tx, mut inbound_clip_rx) = mpsc::channel::<Clip>(100);

    // Clone before moving into IPC — UI uses this for manual text input
    let manual_clip_tx = clip_event_tx.clone();

    // Spawn: IPC pipe listener
    {
        let ipc_client = ipc::IpcClient::new(clip_event_tx, shared.clone());
        rt.spawn(async move {
            if let Err(e) = ipc_client.run().await {
                error!("IPC client fatal error: {e}");
            }
        });
    }

    // Spawn: queue retry processor
    {
        let eng = sync_engine.clone();
        rt.spawn(async move {
            eng.run_queue_processor().await;
        });
    }

    // Spawn: proactive JWT refresh
    {
        let eng = sync_engine.clone();
        rt.spawn(async move {
            eng.run_jwt_refresh_loop().await;
        });
    }

    // Spawn: WebSocket listener with supervisor (restarts after retry-limit backoff)
    {
        let eng = sync_engine.clone();
        let ws_clip_tx = inbound_clip_tx.clone();
        let shared_ws = shared.clone();
        rt.spawn(async move {
            loop {
                match eng.run_ws_listener(ws_clip_tx.clone()).await {
                    Ok(()) => {
                        // Retry limit hit — wait 5 minutes then try the burst again.
                        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                        shared_ws.lock().unwrap().cloud_sync_stopped = false;
                    }
                    Err(e) => {
                        error!("WebSocket listener fatal error: {e}");
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                }
            }
        });
    }

    // Spawn: local clipboard event handler (capture → store → upload → LAN broadcast)
    {
        let storage_ev = storage.clone();
        let settings_ev = settings.clone();
        let sync_ev = sync_engine.clone();
        let shared_ev = shared.clone();
        let lan_tx_ev = lan_clip_tx.clone();
        rt.spawn(async move {
            while let Some(event) = clip_event_rx.recv().await {
                if let Some(content) = event.content {
                    let s = settings_ev.read().await;

                    if !event.source_app.is_empty() {
                        let src = event.source_app.to_lowercase();
                        if s.excluded_apps.iter().any(|app| app.to_lowercase() == src) {
                            shared_ev.lock().unwrap().log(
                                LogLevel::Info,
                                format!("Ignored clip from excluded app: {}", event.source_app),
                            );
                            drop(s);
                            continue;
                        }
                        shared_ev.lock().unwrap().seen_apps.insert(src);
                    }

                    let clip = Clip {
                        id: Uuid::new_v4(),
                        user_id: s.user_id,
                        device_id: s.device_id,
                        content: content.clone(),
                        format: "text".into(),
                        e2e: s.e2e_enabled,
                        created_at: event.timestamp,
                        pin_slot: None,
                    };
                    drop(s);

                    // Add to UI history immediately
                    shared_ev.lock().unwrap().add_clip(clip.clone());

                    if let Err(e) = storage_ev.insert_clip(&clip).await {
                        error!("Failed to store clip: {e}");
                        continue;
                    }
                    if let Err(e) = storage_ev.enqueue_clip(clip.id).await {
                        error!("Failed to enqueue clip: {e}");
                    }
                    match sync_ev.upload_clip(&clip).await {
                        Ok(()) => {}
                        Err(_) => {
                            info!(clip_id = %clip.id, "Queued for retry");
                            shared_ev.lock().unwrap().log(LogLevel::Warn, "Upload failed, queued for retry");
                        }
                    }
                    // Broadcast to LAN peers (best-effort, no block if no receivers)
                    let _ = lan_tx_ev.send(clip);
                }
            }
        });
    }

    // Spawn: inbound clip storage (clips received from other devices)
    {
        let storage_in = storage.clone();
        let settings_in = settings.clone();
        let shared_in = shared.clone();
        rt.spawn(async move {
            while let Some(clip) = inbound_clip_rx.recv().await {
                if let Err(e) = storage_in.insert_clip(&clip).await {
                    error!("Failed to store inbound clip: {e}");
                }
                if settings_in.read().await.auto_paste_inbound {
                    let content = clip.content.clone();
                    let shared_paste = shared_in.clone();
                    tokio::spawn(async move {
                        if let Err(e) = crate::ipc::IpcClient::send_rpc(
                            "set_clipboard",
                            Some(serde_json::json!({ "content": content })),
                        ).await {
                            shared_paste.lock().unwrap().log(
                                LogLevel::Warn,
                                format!("Auto-paste skipped — bridge unreachable: {e}"),
                            );
                        }
                    });
                }
            }
        });
    }

    // Spawn: handle Ctrl+C from terminal by forcing process exit
    rt.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Ctrl+C received, exiting...");
            std::process::exit(0);
        }
    });

    info!("ClipX background tasks running, launching GUI...");

    // Run eframe on the main thread (blocks until window is closed via tray Quit)
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipX — Clipboard History")
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([400.0, 300.0])
            .with_visible(false), // start hidden; tray icon reveals it
        ..Default::default()
    };

    let hotkey_str = rt.block_on(async { settings.read().await.overlay_hotkey.clone() });
    let handle = rt.handle().clone();

    eframe::run_native(
        "ClipX",
        native_options,
        Box::new(move |cc| Ok(Box::new(ClipXApp::new(
            cc,
            shared.clone(),
            storage.clone(),
            settings.clone(),
            handle,
            hotkey_str,
            pair_ui_rx,
            manual_clip_tx,
        )))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    rt.shutdown_background();
    std::process::exit(0);
}

async fn load_or_create_settings(storage: &Storage) -> anyhow::Result<Settings> {
    if let Some(json) = storage.get_setting("config").await? {
        if let Ok(settings) = serde_json::from_str(&json) {
            return Ok(settings);
        }
    }
    let mut settings = Settings::default();
    if let Ok(jwt) = std::env::var("CLIPX_JWT") {
        settings.jwt = Some(jwt);
    }
    if let Ok(uid) = std::env::var("CLIPX_USER_ID") {
        if let Ok(id) = uid.parse() {
            settings.user_id = id;
        }
    }
    if let Ok(did) = std::env::var("CLIPX_DEVICE_ID") {
        if let Ok(id) = did.parse() {
            settings.device_id = id;
        }
    }
    let json = serde_json::to_string(&settings)?;
    storage.save_setting("config", &json).await?;
    Ok(settings)
}
