use std::sync::Arc;
use std::time::{Duration, Instant};
use chrono::Local;
use eframe::egui::{self, Color32, RichText, ScrollArea, ViewportCommand};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use tokio::sync::RwLock;
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tray_icon::menu::{Menu, MenuItem, MenuEvent, PredefinedMenuItem};
use uuid::Uuid;

use std::collections::HashMap;
use crate::hotkeys::{HotkeyAction, HotkeyManager};
use crate::models::{
    BootstrapRequest, BootstrapResponse, ClipboardEvent, Device, InboundPairRequest,
    LanPairRequest, LanPairResponse, PairConfirmRequest, PairConfirmResponse,
    PeerDevice, Settings,
};
use crate::state::{LogLevel, SharedStateHandle};
use crate::storage::Storage;

// ── Screen ────────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum Screen {
    Main,
    Settings,
    Setup,
}

#[derive(PartialEq, Clone)]
enum SetupMode {
    Bootstrap,
    Pair,
}

struct SetupForm {
    mode: SetupMode,
    server_url: String,
    email: String,
    token: String,
    device_name: String,
    loading: bool,
    error: Option<String>,
}

#[derive(PartialEq)]
enum Tab {
    History,
    Devices,
    LanPeers,
    Log,
}

pub struct PairSuccess {
    pub jwt: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub server_url: String,
    pub refresh_token: Option<String>,
    pub jwt_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct ClipXApp {
    state: SharedStateHandle,
    storage: Arc<Storage>,
    settings: Arc<RwLock<Settings>>,
    handle: tokio::runtime::Handle,

    _tray: Option<TrayIcon>,
    _hotkey_manager: Option<HotkeyManager>,
    hotkey_map: HashMap<u32, HotkeyAction>,
    show_item_id: tray_icon::menu::MenuId,
    quit_item_id: tray_icon::menu::MenuId,

    screen: Screen,
    selected_tab: Tab,
    copy_feedback: Option<(String, Instant)>,

    form: SetupForm,
    pair_tx: std::sync::mpsc::SyncSender<Result<PairSuccess, String>>,
    pair_rx: std::sync::mpsc::Receiver<Result<PairSuccess, String>>,

    device_tx: std::sync::mpsc::SyncSender<Result<Vec<Device>, String>>,
    device_rx: std::sync::mpsc::Receiver<Result<Vec<Device>, String>>,
    last_device_fetch: Option<Instant>,

    pair_request_rx: std::sync::mpsc::Receiver<InboundPairRequest>,
    lan_pair_result_tx: std::sync::mpsc::SyncSender<Result<PeerDevice, String>>,
    lan_pair_result_rx: std::sync::mpsc::Receiver<Result<PeerDevice, String>>,
    has_jwt: bool,

    manual_input: String,
    manual_clip_tx: tokio::sync::mpsc::Sender<ClipboardEvent>,

    manual_pair_input: String,

    search_query: String,
    excluded_app_input: String,

    pair_token_tx: std::sync::mpsc::SyncSender<Result<PairTokenInfo, String>>,
    pair_token_rx: std::sync::mpsc::Receiver<Result<PairTokenInfo, String>>,
    pair_token_modal: Option<PairTokenInfo>,
    pair_token_loading: bool,

    /// HWND of the window that had focus right before the hotkey was pressed.
    /// Restored on double-click-paste so Ctrl+V lands in the original app.
    /// Stored as isize because HWND is *const c_void (not Send).
    previous_foreground_hwnd: Option<isize>,
}

#[derive(Debug, Clone)]
pub struct PairTokenInfo {
    pub server_url: String,
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub pair_uri: String,
}

impl ClipXApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        state: SharedStateHandle,
        storage: Arc<Storage>,
        settings: Arc<RwLock<Settings>>,
        handle: tokio::runtime::Handle,
        hotkey_str: String,
        pair_request_rx: std::sync::mpsc::Receiver<InboundPairRequest>,
        manual_clip_tx: tokio::sync::mpsc::Sender<ClipboardEvent>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let (tray, show_item_id, quit_item_id) = match Self::build_tray() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to create tray icon: {e}");
                let dummy = tray_icon::menu::MenuId::default();
                (None, dummy.clone(), dummy)
            }
        };

        let (hotkey_manager, hotkey_map) = match HotkeyManager::new(&hotkey_str) {
            Ok(hm) => {
                let map = hm.hotkey_map().clone();
                state.lock().unwrap().log(LogLevel::Success, format!("Hotkey registered: {hotkey_str}"));
                (Some(hm), map)
            }
            Err(e) => {
                tracing::warn!("Failed to register hotkey: {e}");
                state.lock().unwrap().log(LogLevel::Warn, format!("Hotkey registration failed: {e}"));
                (None, HashMap::new())
            }
        };

        let (current_server_url, has_jwt) = {
            let s = handle.block_on(async { settings.read().await.clone() });
            (s.server_url.clone(), s.jwt.is_some())
        };
        // has_jwt is cached and refreshed each frame — no block_on in render methods

        let screen = Screen::Main;

        let (pair_tx, pair_rx) = std::sync::mpsc::sync_channel(1);
        let (device_tx, device_rx) = std::sync::mpsc::sync_channel(1);
        let (lan_pair_result_tx, lan_pair_result_rx) = std::sync::mpsc::sync_channel(1);
        let (pair_token_tx, pair_token_rx) = std::sync::mpsc::sync_channel(1);

        {
            let mut s = state.lock().unwrap();
            if has_jwt {
                s.log(LogLevel::Info, "ClipX started — use tray icon or Ctrl+Shift+V to open history");
            } else {
                s.log(LogLevel::Info, "ClipX started — setup required");
            }
        }

        let hostname = {
            handle.block_on(async { settings.read().await.device_name.clone() })
        };

        Self {
            state,
            storage,
            settings,
            handle,
            _tray: tray,
            _hotkey_manager: hotkey_manager,
            hotkey_map,
            show_item_id,
            quit_item_id,
            screen,
            selected_tab: Tab::History,
            copy_feedback: None,
            form: SetupForm {
                mode: SetupMode::Pair,
                server_url: current_server_url,
                email: String::new(),
                token: String::new(),
                device_name: hostname,
                loading: false,
                error: None,
            },
            pair_tx,
            pair_rx,
            device_tx,
            device_rx,
            last_device_fetch: None,
            pair_request_rx,
            lan_pair_result_tx,
            lan_pair_result_rx,
            has_jwt,
            manual_input: String::new(),
            manual_clip_tx,
            manual_pair_input: String::new(),
            search_query: String::new(),
            excluded_app_input: String::new(),
            pair_token_tx,
            pair_token_rx,
            pair_token_modal: None,
            pair_token_loading: false,
            previous_foreground_hwnd: None,
        }
    }

    fn build_tray() -> anyhow::Result<(Option<TrayIcon>, tray_icon::menu::MenuId, tray_icon::menu::MenuId)> {
        let icon_rgba: Vec<u8> = (0..32 * 32)
            .flat_map(|i| {
                let x = i % 32;
                let y = i / 32;
                let in_circle = ((x as i32 - 16).pow(2) + (y as i32 - 16).pow(2)) < 14_i32.pow(2);
                if in_circle { [30u8, 144, 255, 255] } else { [0, 0, 0, 0] }
            })
            .collect();
        let icon = tray_icon::Icon::from_rgba(icon_rgba, 32, 32)?;

        let show_item = MenuItem::new("Show / Hide", true, None);
        let quit_item = MenuItem::new("Quit ClipX", true, None);
        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();

        let menu = Menu::new();
        menu.append(&show_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ClipX — Clipboard Sync")
            .with_icon(icon)
            .build()?;

        Ok((Some(tray), show_id, quit_id))
    }

    /// Apply a mutation to settings in memory and persist asynchronously.
    fn update_settings<F>(&self, f: F)
    where
        F: FnOnce(&mut Settings) + Send + 'static,
    {
        let storage = self.storage.clone();
        let settings = self.settings.clone();
        self.handle.spawn(async move {
            let mut s = settings.write().await;
            f(&mut *s);
            let json = serde_json::to_string(&*s).unwrap_or_default();
            drop(s);
            let _ = storage.save_setting("config", &json).await;
        });
    }

    fn fetch_devices(&self) {
        let settings = self.settings.clone();
        let state = self.state.clone();
        let tx = self.device_tx.clone();

        state.lock().unwrap().devices_loading = true;

        self.handle.spawn(async move {
            let s = settings.read().await;
            let jwt = match &s.jwt {
                Some(j) => j.clone(),
                None => { let _ = tx.try_send(Err("Not authenticated".into())); return; }
            };
            let url = format!("{}/api/v1/devices", s.server_url);
            drop(s);

            let result = reqwest::Client::new()
                .get(&url)
                .bearer_auth(&jwt)
                .timeout(Duration::from_secs(10))
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<Vec<Device>>().await {
                        Ok(devices) => { let _ = tx.try_send(Ok(devices)); }
                        Err(e) => { let _ = tx.try_send(Err(format!("Parse error: {e}"))); }
                    }
                }
                Ok(resp) => { let _ = tx.try_send(Err(format!("HTTP {}", resp.status()))); }
                Err(e) => { let _ = tx.try_send(Err(format!("Network error: {e}"))); }
            }
        });
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for ClipXApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tray menu events
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id() == &self.quit_item_id {
                std::process::exit(0);
            } else if event.id() == &self.show_item_id {
                let visible = ctx.input(|i| i.viewport().inner_rect.is_some());
                ctx.send_viewport_cmd(ViewportCommand::Visible(!visible));
                if !visible { ctx.send_viewport_cmd(ViewportCommand::Focus); }
            }
        }

        // Tray icon left-click
        while TrayIconEvent::receiver().try_recv().is_ok() {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }

        // Global hotkey
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            match self.hotkey_map.get(&event.id) {
                Some(HotkeyAction::ToggleOverlay) => {
                    self.previous_foreground_hwnd = capture_foreground_hwnd();
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.selected_tab = Tab::History;
                }
                Some(HotkeyAction::PasteSlot(slot)) => {
                    let slot = *slot;
                    let content = {
                        let s = self.state.lock().unwrap();
                        s.clip_for_slot(slot).map(|c| c.content.clone())
                    };
                    if let Some(content) = content {
                        let target_hwnd = capture_foreground_hwnd();
                        self.handle.spawn(async move {
                            match crate::ipc::IpcClient::send_rpc(
                                "set_clipboard",
                                Some(serde_json::json!({ "content": content })),
                            ).await {
                                Ok(_) => {
                                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                                    if let Some(h) = target_hwnd {
                                        restore_foreground(h);
                                        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                                    }
                                    crate::overlay::simulate_paste();
                                }
                                Err(e) => {
                                    tracing::warn!("Slot {slot} paste failed — bridge unreachable: {e}");
                                }
                            }
                        });
                    }
                }
                None => {}
            }
        }

        // Refresh cached JWT status once per frame (non-blocking try_read)
        if let Ok(s) = self.settings.try_read() {
            self.has_jwt = s.jwt.is_some();
        }

        // X button always hides; quit happens via tray (direct process::exit)
        ctx.input(|i| {
            if i.viewport().close_requested() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
        });

        // Clear copy feedback
        if matches!(&self.copy_feedback, Some((_, t)) if t.elapsed() > Duration::from_millis(1500)) {
            self.copy_feedback = None;
        }

        // Handle pair result
        if let Ok(result) = self.pair_rx.try_recv() {
            self.form.loading = false;
            match result {
                Ok(success) => {
                    // Persist to DB and update in-memory settings
                    let storage = self.storage.clone();
                    let settings = self.settings.clone();
                    let jwt = success.jwt.clone();
                    let user_id = success.user_id;
                    let device_id = success.device_id;
                    let server_url = success.server_url.clone();
                    let refresh = success.refresh_token.clone();
                    let exp = success.jwt_expires_at;
                    let state = self.state.clone();

                    self.handle.block_on(async {
                        let mut s = settings.write().await;
                        s.jwt = Some(jwt);
                        s.refresh_token = refresh;
                        s.jwt_expires_at = exp;
                        s.user_id = user_id;
                        s.device_id = device_id;
                        s.server_url = server_url;
                        let json = serde_json::to_string(&*s).unwrap_or_default();
                        drop(s);
                        let _ = storage.save_setting("config", &json).await;
                    });

                    self.has_jwt = true;
                    if self.screen == Screen::Setup {
                        self.screen = Screen::Main;
                    }
                    state.lock().unwrap().log(LogLevel::Success, "Paired successfully! Connecting…");
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    self.fetch_devices();
                }
                Err(e) => {
                    self.form.error = Some(e);
                }
            }
        }

        // Handle pair-token result
        if let Ok(result) = self.pair_token_rx.try_recv() {
            self.pair_token_loading = false;
            match result {
                Ok(info) => {
                    self.state.lock().unwrap().log(
                        LogLevel::Success,
                        format!("Generated pair code (expires {})", info.expires_at.with_timezone(&Local).format("%H:%M:%S")),
                    );
                    self.pair_token_modal = Some(info);
                }
                Err(e) => {
                    self.state.lock().unwrap().log(LogLevel::Error, format!("Pair code failed: {e}"));
                }
            }
        }

        // Handle device fetch result
        if let Ok(result) = self.device_rx.try_recv() {
            let mut s = self.state.lock().unwrap();
            s.devices_loading = false;
            match result {
                Ok(devices) => {
                    s.log(LogLevel::Info, format!("Devices refreshed ({} online)", devices.len()));
                    s.devices = devices;
                    s.devices_fetched_at = Some(Instant::now());
                }
                Err(e) => {
                    s.log(LogLevel::Warn, format!("Device fetch failed: {e}"));
                }
            }
            self.last_device_fetch = Some(Instant::now());
        }

        // Poll for incoming LAN pairing requests
        if let Ok(req) = self.pair_request_rx.try_recv() {
            self.state.lock().unwrap().pending_pair_request = Some(req);
        }

        // Poll for LAN pair result (accept callback)
        if let Ok(result) = self.lan_pair_result_rx.try_recv() {
            match result {
                Ok(peer) => {
                    let name = peer.name.clone();
                    let storage = self.storage.clone();
                    let state = self.state.clone();
                    self.handle.spawn(async move {
                        if let Err(e) = storage.insert_peer_device(&peer).await {
                            tracing::error!("Failed to store LAN peer: {e}");
                            return;
                        }
                        state.lock().unwrap().peer_devices.push(peer);
                        state.lock().unwrap().log(LogLevel::Success, format!("Paired with LAN peer: {name}"));
                    });
                    self.state.lock().unwrap().pending_pair_request = None;
                }
                Err(e) => {
                    self.state.lock().unwrap().log(LogLevel::Error, format!("LAN pairing failed: {e}"));
                    self.state.lock().unwrap().pending_pair_request = None;
                }
            }
        }

        // Show modal for pending pairing request (rendered over main UI)
        if self.state.lock().unwrap().pending_pair_request.is_some() {
            self.render_pair_request_modal(ctx);
        }

        // Show modal for freshly-minted pair code
        if self.pair_token_modal.is_some() {
            self.render_pair_token_modal(ctx);
        }

        // Auto-refresh devices every 30s when on Devices tab
        if self.screen == Screen::Main && self.selected_tab == Tab::Devices {
            let stale = self.last_device_fetch
                .map(|t| t.elapsed() > Duration::from_secs(30))
                .unwrap_or(true);
            let loading = self.state.lock().unwrap().devices_loading;
            if stale && !loading {
                self.fetch_devices();
            }
        }

        ctx.request_repaint_after(Duration::from_millis(200));

        match self.screen {
            Screen::Main => {
                self.render_menu_bar(ctx);
                self.render_status_bar(ctx);
                self.render_reauth_banner(ctx);
                self.render_tab_bar(ctx);
                self.render_main_panel(ctx);
            }
            Screen::Settings => {
                self.render_menu_bar(ctx);
                self.render_status_bar(ctx);
                self.render_reauth_banner(ctx);
                self.render_settings_screen(ctx);
            }
            Screen::Setup => {
                self.render_setup(ctx);
            }
        }
    }
}

// ── Setup screen ──────────────────────────────────────────────────────────────

impl ClipXApp {
    fn render_setup(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("setup_back_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back").clicked() {
                    self.screen = Screen::Main;
                }
                ui.separator();
                ui.label(RichText::new("Cloud Setup").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("LAN sync keeps working without this")
                            .color(Color32::GRAY)
                            .small(),
                    );
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(RichText::new("ClipX Setup").size(28.0).strong());
                ui.add_space(8.0);
                ui.label(RichText::new("Connect this device to your clipboard sync network").color(Color32::GRAY));
                ui.add_space(32.0);

                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(24.0))
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);

                        // Mode selector
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.form.mode, SetupMode::Pair, "Join with Token");
                            ui.selectable_value(&mut self.form.mode, SetupMode::Bootstrap, "First Device");
                        });
                        ui.separator();
                        ui.add_space(8.0);

                        let field_w = 372.0;

                        // Server URL (shared)
                        ui.label("Server URL");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.form.server_url)
                                .hint_text("http://your-server:8443")
                                .desired_width(field_w),
                        );
                        ui.add_space(8.0);

                        // Device name (shared)
                        ui.label("Device Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.form.device_name)
                                .hint_text("My PC")
                                .desired_width(field_w),
                        );
                        ui.add_space(8.0);

                        match self.form.mode.clone() {
                            SetupMode::Pair => {
                                ui.label("Pairing Token");
                                ui.label(
                                    RichText::new("Get this from another paired device via the API")
                                        .color(Color32::GRAY)
                                        .small(),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.form.token)
                                        .hint_text("Paste token here…")
                                        .desired_width(field_w),
                                );
                                ui.add_space(16.0);

                                let ready = !self.form.token.is_empty()
                                    && !self.form.server_url.is_empty()
                                    && !self.form.device_name.is_empty()
                                    && !self.form.loading;

                                let btn = ui.add_enabled(
                                    ready,
                                    egui::Button::new(
                                        if self.form.loading { "Pairing…" } else { "Pair Device" }
                                    ).min_size(egui::vec2(field_w, 36.0)),
                                );

                                if btn.clicked() {
                                    self.submit_pair();
                                }
                            }
                            SetupMode::Bootstrap => {
                                ui.label("Email");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.form.email)
                                        .hint_text("you@example.com")
                                        .desired_width(field_w),
                                );
                                ui.add_space(16.0);

                                let ready = !self.form.email.is_empty()
                                    && !self.form.server_url.is_empty()
                                    && !self.form.device_name.is_empty()
                                    && !self.form.loading;

                                let btn = ui.add_enabled(
                                    ready,
                                    egui::Button::new(
                                        if self.form.loading { "Creating…" } else { "Create Account & Pair" }
                                    ).min_size(egui::vec2(field_w, 36.0)),
                                );

                                if btn.clicked() {
                                    self.submit_bootstrap();
                                }
                            }
                        }

                        if let Some(err) = &self.form.error {
                            ui.add_space(8.0);
                            ui.label(RichText::new(err).color(Color32::RED));
                        }
                    });

                if self.form.mode == SetupMode::Pair {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(
                            "Generate a token on a paired device:\n  curl -X POST http://server:8443/api/v1/pair/request \\\n    -H \"Authorization: Bearer <jwt>\""
                        )
                        .color(Color32::GRAY)
                        .small()
                        .monospace(),
                    );
                }
            });
        });
    }

    fn submit_pair(&mut self) {
        self.form.loading = true;
        self.form.error = None;

        let server_url = self.form.server_url.trim_end_matches('/').to_string();
        let token = self.form.token.clone();
        let device_name = self.form.device_name.clone();
        let tx = self.pair_tx.clone();

        self.handle.spawn(async move {
            let result = reqwest::Client::new()
                .post(format!("{server_url}/api/v1/pair/confirm"))
                .json(&PairConfirmRequest { token, device_name })
                .timeout(Duration::from_secs(15))
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().as_u16() == 200 => {
                    match resp.json::<PairConfirmResponse>().await {
                        Ok(body) => {
                            let _ = tx.try_send(Ok(PairSuccess {
                                jwt: body.jwt,
                                user_id: body.user_id,
                                device_id: body.device_id,
                                server_url,
                                refresh_token: body.refresh_token,
                                jwt_expires_at: body.jwt_expires_at,
                            }));
                        }
                        Err(e) => { let _ = tx.try_send(Err(format!("Bad response: {e}"))); }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let _ = tx.try_send(Err(format!("Server error {status}: {body}")));
                }
                Err(e) => { let _ = tx.try_send(Err(format!("Connection failed: {e}"))); }
            }
        });
    }

    fn submit_bootstrap(&mut self) {
        self.form.loading = true;
        self.form.error = None;

        let server_url = self.form.server_url.trim_end_matches('/').to_string();
        let email = self.form.email.clone();
        let device_name = self.form.device_name.clone();
        let tx = self.pair_tx.clone();

        self.handle.spawn(async move {
            let result = reqwest::Client::new()
                .post(format!("{server_url}/api/v1/bootstrap"))
                .json(&BootstrapRequest { email, device_name })
                .timeout(Duration::from_secs(15))
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().as_u16() == 200 => {
                    match resp.json::<BootstrapResponse>().await {
                        Ok(body) => {
                            let _ = tx.try_send(Ok(PairSuccess {
                                jwt: body.jwt,
                                user_id: body.user_id,
                                device_id: body.device_id,
                                server_url,
                                refresh_token: body.refresh_token,
                                jwt_expires_at: body.jwt_expires_at,
                            }));
                        }
                        Err(e) => { let _ = tx.try_send(Err(format!("Bad response: {e}"))); }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let _ = tx.try_send(Err(format!("Server error {status}: {body}")));
                }
                Err(e) => { let _ = tx.try_send(Err(format!("Connection failed: {e}"))); }
            }
        });
    }
}

// ── Main screen ───────────────────────────────────────────────────────────────

impl ClipXApp {
    fn render_status_bar(&self, ctx: &egui::Context) {
        let has_jwt = self.has_jwt;

        egui::TopBottomPanel::top("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ClipX").strong());
                ui.separator();

                let s = self.state.lock().unwrap();

                let pipe_color = if s.pipe_connected {
                    Color32::from_rgb(40, 200, 80)
                } else {
                    Color32::from_rgb(220, 50, 50)
                };
                status_dot(ui, pipe_color);
                ui.label(if s.pipe_connected { "Bridge" } else { "Bridge (disconnected)" });

                ui.separator();

                let (ws_color, ws_label) = if s.ws_connected {
                    (Color32::from_rgb(40, 200, 80), "Cloud")
                } else if s.cloud_sync_stopped {
                    (Color32::from_rgb(220, 50, 50), "Cloud: retry limit hit")
                } else if has_jwt {
                    (Color32::from_rgb(220, 180, 30), "Cloud (connecting…)")
                } else {
                    (Color32::from_rgb(100, 100, 110), "Cloud: off")
                };
                status_dot(ui, ws_color);
                ui.label(ws_label);

                ui.separator();

                let lan_online = s.peer_devices.iter().filter(|p| p.online).count();
                let lan_color = if !s.lan_server_running {
                    Color32::from_rgb(100, 100, 100)
                } else if lan_online > 0 {
                    Color32::from_rgb(40, 200, 80)
                } else {
                    Color32::from_rgb(100, 150, 220)
                };
                status_dot(ui, lan_color);
                ui.label(if !s.lan_server_running {
                    "LAN (off)".to_string()
                } else if lan_online > 0 {
                    format!("LAN ({lan_online} online)")
                } else {
                    "LAN".to_string()
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("↑{}  ↓{}", s.upload_count, s.receive_count))
                            .color(Color32::GRAY)
                            .small(),
                    );
                });
            });
        });
    }

    fn render_reauth_banner(&mut self, ctx: &egui::Context) {
        if self.has_jwt {
            return;
        }
        let panel_frame = egui::Frame::default()
            .fill(Color32::from_rgb(80, 50, 0))
            .inner_margin(egui::Margin::symmetric(12.0, 8.0));
        egui::TopBottomPanel::top("reauth_banner")
            .frame(panel_frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⚠  Cloud sync not authenticated")
                            .color(Color32::from_rgb(255, 220, 120))
                            .strong(),
                    );
                    ui.label(
                        RichText::new("— LAN sync still works. Re-pair to resume cloud delivery.")
                            .color(Color32::from_rgb(220, 200, 160))
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Re-pair").clicked() {
                            self.screen = Screen::Setup;
                        }
                    });
                });
            });
    }

    fn render_tab_bar(&mut self, ctx: &egui::Context) {
        let (clip_count, device_count, paired_count, log_count) = {
            let s = self.state.lock().unwrap();
            (s.clips.len(), s.devices.len(), s.peer_devices.len(), s.log.len())
        };

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, Tab::History, format!("History ({})", clip_count));
                if ui.selectable_value(&mut self.selected_tab, Tab::Devices,
                    format!("Devices ({})", device_count)).clicked()
                {
                    let stale = self.last_device_fetch
                        .map(|t| t.elapsed() > Duration::from_secs(5))
                        .unwrap_or(true);
                    if stale && !self.state.lock().unwrap().devices_loading {
                        self.fetch_devices();
                    }
                }
                ui.selectable_value(&mut self.selected_tab, Tab::LanPeers, format!("LAN Peers ({})", paired_count));
                ui.selectable_value(&mut self.selected_tab, Tab::Log, format!("Log ({})", log_count));
            });
        });
    }

    fn render_main_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| match self.selected_tab {
            Tab::History => self.render_history(ui),
            Tab::Devices => self.render_devices(ui),
            Tab::LanPeers => self.render_lan_peers(ui),
            Tab::Log => self.render_log(ui),
        });
    }

    fn render_history(&mut self, ui: &mut egui::Ui) {
        // Manual text input
        ui.horizontal(|ui| {
            let input = egui::TextEdit::singleline(&mut self.manual_input)
                .hint_text("Type text to add to history…")
                .desired_width(ui.available_width() - 70.0);
            let response = ui.add(input);
            let submit = ui.add_enabled(
                !self.manual_input.is_empty(),
                egui::Button::new("Add").min_size(egui::vec2(60.0, 0.0)),
            );
            let enter_pressed = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if (submit.clicked() || enter_pressed) && !self.manual_input.is_empty() {
                let content = std::mem::take(&mut self.manual_input);
                let tx = self.manual_clip_tx.clone();
                self.handle.spawn(async move {
                    let _ = tx.send(ClipboardEvent {
                        event_type: "manual".into(),
                        content: Some(content),
                        format: "text".into(),
                        timestamp: chrono::Utc::now(),
                        source_app: String::new(),
                    }).await;
                });
            }
        });
        ui.separator();

        // Search box
        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Search history…")
                    .desired_width(ui.available_width() - 70.0),
            );
            if !self.search_query.is_empty() && ui.button("Clear").clicked() {
                self.search_query.clear();
            }
        });
        ui.separator();

        if let Some((text, _)) = &self.copy_feedback {
            let preview = text.chars().take(40).collect::<String>();
            ui.label(RichText::new(format!("✓ Copied: {}…", preview)).color(Color32::GREEN));
            ui.separator();
        }

        let all_clips: Vec<_> = self.state.lock().unwrap().clips.iter().cloned().collect();

        // Filter by search query (case-insensitive substring), then sort pinned-first.
        let query = self.search_query.to_lowercase();
        let mut clips: Vec<_> = if query.is_empty() {
            all_clips
        } else {
            all_clips
                .into_iter()
                .filter(|c| c.content.to_lowercase().contains(&query))
                .collect()
        };
        clips.sort_by(|a, b| {
            b.is_pinned()
                .cmp(&a.is_pinned())
                .then_with(|| a.pin_slot.cmp(&b.pin_slot))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });

        if clips.is_empty() {
            ui.centered_and_justified(|ui| {
                let msg = if self.search_query.is_empty() {
                    "No clipboard history yet"
                } else {
                    "No clips match your search"
                };
                ui.label(RichText::new(msg).color(Color32::GRAY));
            });
            return;
        }

        let mut to_delete: Option<uuid::Uuid> = None;
        let mut to_set_slot: Option<(uuid::Uuid, Option<u8>)> = None;

        ScrollArea::vertical().show(ui, |ui| {
            for (i, clip) in clips.iter().enumerate() {
                let preview = clip.content.chars().take(100).collect::<String>();
                let preview = if clip.content.len() > 100 { format!("{}…", preview) } else { preview };
                let time_str = clip.created_at.with_timezone(&Local).format("%H:%M:%S").to_string();

                ui.push_id(i, |ui| {
                    let frame_resp = egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(slot) = clip.pin_slot {
                                ui.label(
                                    RichText::new(format!("Ctrl+{slot}"))
                                        .small()
                                        .strong()
                                        .color(Color32::from_rgb(255, 200, 60))
                                ).on_hover_text(format!("Quick Paste Slot {slot}"));
                            }
                            ui.label(RichText::new(&time_str).color(Color32::GRAY).small());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("✕").on_hover_text("Delete").clicked() {
                                    to_delete = Some(clip.id);
                                }
                                let current_slot = clip.pin_slot;
                                let clip_id = clip.id;
                                egui::ComboBox::from_id_salt(format!("slot_{}", clip.id))
                                    .selected_text(match current_slot {
                                        Some(s) => format!("Slot {s}"),
                                        None => "\u{2014}".into(),
                                    })
                                    .width(70.0)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(current_slot.is_none(), "None").clicked() && current_slot.is_some() {
                                            to_set_slot = Some((clip_id, None));
                                        }
                                        for s in 1u8..=9 {
                                            let occupied = clips.iter().any(|c| c.pin_slot == Some(s) && c.id != clip_id);
                                            let label = if occupied {
                                                format!("Slot {s} (replace)")
                                            } else {
                                                format!("Slot {s}")
                                            };
                                            if ui.selectable_label(current_slot == Some(s), label).clicked() && current_slot != Some(s) {
                                                to_set_slot = Some((clip_id, Some(s)));
                                            }
                                        }
                                    });
                                ui.label(RichText::new(format!("{} chars", clip.content.len())).color(Color32::GRAY).small());
                            });
                        });
                        ui.label(&preview);
                    });

                    // Double-click = Windows+V parity: copy + hide overlay + restore
                    // previous foreground window + synthesize Ctrl+V.
                    if frame_resp.response.double_clicked() {
                        let content = clip.content.clone();
                        let target_hwnd = self.previous_foreground_hwnd.take();
                        self.handle.spawn(async move {
                            // 1. Write to clipboard via the .NET bridge.
                            let _ = crate::ipc::IpcClient::send_rpc(
                                "set_clipboard",
                                Some(serde_json::json!({ "content": content })),
                            ).await;
                            // 2. Restore the target window + simulate Ctrl+V.
                            //    A tiny sleep gives Windows time to settle the
                            //    foreground change before key events go out.
                            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                            if let Some(h) = target_hwnd {
                                restore_foreground(h);
                                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                            }
                            crate::overlay::simulate_paste();
                        });
                        ui.ctx().send_viewport_cmd(ViewportCommand::Visible(false));
                    } else if frame_resp.response.clicked() {
                        // Single click = copy only (no auto-paste).
                        let content = clip.content.clone();
                        self.handle.spawn(async move {
                            let _ = crate::ipc::IpcClient::send_rpc(
                                "set_clipboard",
                                Some(serde_json::json!({ "content": content })),
                            ).await;
                        });
                        self.copy_feedback = Some((clip.content.clone(), Instant::now()));
                    }
                    if frame_resp.response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                });
                ui.add_space(2.0);
            }
        });

        if let Some((id, slot)) = to_set_slot {
            {
                let mut s = self.state.lock().unwrap();
                s.assign_slot(id, slot);
            }
            let storage = self.storage.clone();
            self.handle.spawn(async move {
                if let Some(s) = slot {
                    let _ = storage.clear_pin_slot(s).await;
                }
                let _ = storage.set_pin_slot(id, slot).await;
            });
        }

        if let Some(id) = to_delete {
            self.state.lock().unwrap().clips.retain(|c| c.id != id);
            let storage = self.storage.clone();
            self.handle.spawn(async move {
                let _ = storage.delete_clip(id).await;
            });
        }
    }

    fn render_devices(&mut self, ui: &mut egui::Ui) {
        let (devices, loading) = {
            let s = self.state.lock().unwrap();
            (s.devices.clone(), s.devices_loading)
        };

        let current_device_id = self.handle.block_on(async {
            self.settings.read().await.device_id
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Paired Devices").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = ui.add_enabled(!loading, egui::Button::new(if loading { "Refreshing…" } else { "↺ Refresh" }));
                if btn.clicked() {
                    self.fetch_devices();
                }
                if let Some(t) = self.last_device_fetch {
                    ui.label(RichText::new(format!("{}s ago", t.elapsed().as_secs())).color(Color32::GRAY).small());
                }
            });
        });
        ui.separator();

        if devices.is_empty() && !loading {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No devices found").color(Color32::GRAY));
            });
            return;
        }

        ScrollArea::vertical().show(ui, |ui| {
            for device in &devices {
                let is_current = device.id == current_device_id;
                let is_revoked = device.revoked;

                let frame_color = if is_current {
                    Color32::from_rgb(30, 70, 120)
                } else if is_revoked {
                    Color32::from_rgb(60, 30, 30)
                } else {
                    Color32::from_rgb(40, 40, 45)
                };

                egui::Frame::none()
                    .fill(frame_color)
                    .rounding(6.0)
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Status dot
                            let dot_color = if is_revoked {
                                Color32::from_rgb(180, 50, 50)
                            } else if is_current {
                                Color32::from_rgb(40, 200, 80)
                            } else {
                                Color32::from_rgb(100, 180, 255)
                            };
                            status_dot(ui, dot_color);

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&device.name).strong());
                                    if is_current {
                                        ui.label(RichText::new("(this device)").color(Color32::from_rgb(100, 180, 255)).small());
                                    }
                                    if is_revoked {
                                        ui.label(RichText::new("REVOKED").color(Color32::RED).small().strong());
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let paired = device.paired_at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string();
                                    ui.label(RichText::new(format!("Paired: {paired}")).color(Color32::GRAY).small());
                                    ui.label(RichText::new(format!("ID: {}…", &device.id.to_string()[..8])).color(Color32::GRAY).small());
                                });
                            });
                        });
                    });

                ui.add_space(4.0);
            }
        });

        ui.add_space(16.0);
        ui.separator();

        if !self.has_jwt {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Cloud sync is not configured.")
                                .color(Color32::from_rgb(220, 180, 60)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Set up cloud sync").clicked() {
                                self.screen = Screen::Setup;
                            }
                        });
                    });
                });
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Add another device").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.pair_token_loading { "Generating…" } else { "Generate Pair Code" };
                    let btn = ui.add_enabled(!self.pair_token_loading, egui::Button::new(label));
                    if btn.clicked() {
                        self.request_pair_token();
                    }
                });
            });
            ui.label(
                RichText::new("Generates a 10-minute single-use code. Scan the QR with a mobile/extension that supports clipx://pair URIs, or copy the code below into the new device's Setup screen.")
                    .color(Color32::GRAY)
                    .small(),
            );
        }
    }

    fn request_pair_token(&mut self) {
        self.pair_token_loading = true;
        let server_url = self.handle.block_on(async { self.settings.read().await.server_url.clone() });
        let jwt = self.handle.block_on(async { self.settings.read().await.jwt.clone() });
        let tx = self.pair_token_tx.clone();

        self.handle.spawn(async move {
            let Some(jwt) = jwt else {
                let _ = tx.try_send(Err("Not authenticated — set up cloud sync first.".into()));
                return;
            };
            #[derive(serde::Deserialize)]
            struct Resp {
                token: String,
                expires_at: chrono::DateTime<chrono::Utc>,
            }
            let client = reqwest::Client::new();
            let res = client
                .post(format!("{server_url}/api/v1/pair/request"))
                .bearer_auth(&jwt)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match res {
                Ok(resp) if resp.status().is_success() => match resp.json::<Resp>().await {
                    Ok(body) => {
                        let pair_uri = format!(
                            "clipx://pair?server={}&token={}",
                            urlencode(&server_url),
                            body.token,
                        );
                        let _ = tx.try_send(Ok(PairTokenInfo {
                            server_url,
                            token: body.token,
                            expires_at: body.expires_at,
                            pair_uri,
                        }));
                    }
                    Err(e) => { let _ = tx.try_send(Err(format!("Bad response: {e}"))); }
                },
                Ok(resp) => {
                    let s = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let _ = tx.try_send(Err(format!("Server returned {s}: {body}")));
                }
                Err(e) => { let _ = tx.try_send(Err(format!("Request failed: {e}"))); }
            }
        });
    }

    fn render_cloud_setup_form(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.form.mode, SetupMode::Pair, "Join with Token");
            ui.selectable_value(&mut self.form.mode, SetupMode::Bootstrap, "First Device");
        });
        ui.separator();
        ui.add_space(6.0);

        ui.label("Server URL");
        ui.add(egui::TextEdit::singleline(&mut self.form.server_url)
            .hint_text("http://your-server:8443")
            .desired_width(f32::INFINITY));
        ui.add_space(4.0);

        ui.label("Device Name");
        ui.add(egui::TextEdit::singleline(&mut self.form.device_name)
            .desired_width(f32::INFINITY));
        ui.add_space(4.0);

        match self.form.mode.clone() {
            SetupMode::Pair => {
                ui.label("Pairing Token");
                ui.add(egui::TextEdit::singleline(&mut self.form.token)
                    .hint_text("Paste token…")
                    .desired_width(f32::INFINITY));
                ui.add_space(8.0);
                let ready = !self.form.token.is_empty() && !self.form.server_url.is_empty() && !self.form.loading;
                let btn = ui.add_enabled(ready, egui::Button::new(
                    if self.form.loading { "Pairing…" } else { "Pair Device" }
                ).min_size(egui::vec2(f32::INFINITY, 32.0)));
                if btn.clicked() { self.submit_pair(); }
            }
            SetupMode::Bootstrap => {
                ui.label("Email");
                ui.add(egui::TextEdit::singleline(&mut self.form.email)
                    .hint_text("you@example.com")
                    .desired_width(f32::INFINITY));
                ui.add_space(8.0);
                let ready = !self.form.email.is_empty() && !self.form.server_url.is_empty() && !self.form.loading;
                let btn = ui.add_enabled(ready, egui::Button::new(
                    if self.form.loading { "Creating…" } else { "Create Account" }
                ).min_size(egui::vec2(f32::INFINITY, 32.0)));
                if btn.clicked() { self.submit_bootstrap(); }
            }
        }

        if let Some(err) = &self.form.error {
            ui.add_space(4.0);
            ui.label(RichText::new(err).color(Color32::RED).small());
        }
    }

    fn render_lan_peers(&mut self, ui: &mut egui::Ui) {
        let paired = self.state.lock().unwrap().peer_devices.clone();

        let (my_device_id, my_name, my_pub_key, my_port) = self.handle.block_on(async {
            let s = self.settings.read().await;
            (s.device_id, s.device_name.clone(), s.public_key.clone(), s.local_sync_port)
        });

        // ── Pair by IP ───────────────────────────────────────────────────────
        ui.label(RichText::new("Pair by IP").strong());
        ui.separator();
        ui.label(
            RichText::new("Enter the other device's LAN address to send a pairing request.")
                .color(Color32::GRAY).small(),
        );
        ui.horizontal(|ui| {
            let hint = format!("192.168.0.10 or 192.168.0.10:{my_port}");
            ui.add(
                egui::TextEdit::singleline(&mut self.manual_pair_input)
                    .hint_text(hint)
                    .desired_width(ui.available_width() - 110.0),
            );
            let ready = !self.manual_pair_input.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Request Pair")
                .min_size(egui::vec2(100.0, 0.0))).clicked()
            {
                match parse_ip_port(self.manual_pair_input.trim(), my_port) {
                    Ok((ip, port)) => {
                        let state = self.state.clone();
                        let my_ip = local_ip();
                        let my_name2 = my_name.clone();
                        let my_pub_key2 = my_pub_key.clone();
                        self.manual_pair_input.clear();
                        self.handle.spawn(async move {
                            let url = format!("http://{ip}:{port}/pair/request");
                            let req = LanPairRequest {
                                from_device_id: my_device_id,
                                from_name: my_name2,
                                from_public_key: my_pub_key2,
                                ip: my_ip,
                                port: my_port,
                                session_token: crate::models::generate_session_token(),
                            };
                            match reqwest::Client::new()
                                .post(&url)
                                .json(&req)
                                .timeout(std::time::Duration::from_secs(5))
                                .send()
                                .await
                            {
                                Ok(r) if r.status().as_u16() == 202 => {
                                    state.lock().unwrap().log(LogLevel::Info,
                                        format!("Pair request sent to {ip}:{port} — waiting for accept"));
                                }
                                Ok(r) if r.status().as_u16() == 409 => {
                                    state.lock().unwrap().log(LogLevel::Warn,
                                        format!("{ip}:{port} reports we're already paired"));
                                }
                                Ok(r) => {
                                    state.lock().unwrap().log(LogLevel::Warn,
                                        format!("Pair request rejected by {ip}:{port}: HTTP {}", r.status()));
                                }
                                Err(e) => {
                                    state.lock().unwrap().log(LogLevel::Error,
                                        format!("Could not reach {ip}:{port}: {e}"));
                                }
                            }
                        });
                    }
                    Err(e) => {
                        self.state.lock().unwrap().log(LogLevel::Error,
                            format!("Invalid address: {e}"));
                    }
                }
            }
        });

        ui.add_space(16.0);

        // ── Trusted LAN peers ────────────────────────────────────────────────
        ui.label(RichText::new("Paired LAN Devices").strong());
        ui.separator();

        if paired.is_empty() {
            ui.label(RichText::new("No paired LAN peers yet").color(Color32::GRAY).small());
        }

        let mut to_forget: Option<uuid::Uuid> = None;

        egui::ScrollArea::vertical().id_salt("lan_paired").show(ui, |ui| {
            for peer in &paired {
                let dot_color = if peer.online {
                    Color32::from_rgb(40, 200, 80)
                } else {
                    Color32::from_rgb(120, 120, 130)
                };

                egui::Frame::none()
                    .fill(Color32::from_rgb(40, 40, 50))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            status_dot(ui, dot_color);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&peer.name).strong());
                                ui.label(RichText::new(
                                    format!("{}:{}  paired {}",
                                        peer.ip, peer.port,
                                        peer.paired_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")))
                                    .color(Color32::GRAY).small());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("Forget").clicked() {
                                    to_forget = Some(peer.id);
                                }
                            });
                        });
                    });
                ui.add_space(3.0);
            }
        });

        if let Some(forget_id) = to_forget {
            let storage = self.storage.clone();
            let state = self.state.clone();
            self.handle.spawn(async move {
                let _ = storage.delete_peer_device(forget_id).await;
                let mut s = state.lock().unwrap();
                s.peer_devices.retain(|p| p.id != forget_id);
                s.log(LogLevel::Info, "LAN peer removed");
            });
        }
    }

    fn render_pair_request_modal(&mut self, ctx: &egui::Context) {
        let req_clone = {
            let s = self.state.lock().unwrap();
            s.pending_pair_request.clone()
        };

        let req = match req_clone {
            Some(r) => r,
            None => return,
        };

        egui::Window::new("LAN Pairing Request")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .min_width(340.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("\"{}\" wants to sync", req.request.from_name)).size(16.0).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("IP: {}:{}", req.request.ip, req.request.port))
                        .color(Color32::GRAY).monospace().small());
                    ui.label(RichText::new(format!("ID: {}", req.request.from_device_id))
                        .color(Color32::GRAY).monospace().small());
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        let tx = self.lan_pair_result_tx.clone();
                        let req2 = req.clone();
                        let settings = self.settings.clone();
                        let state = self.state.clone();

                        if ui.add(egui::Button::new(
                            RichText::new("Accept").color(Color32::BLACK))
                            .fill(Color32::from_rgb(40, 200, 80))
                            .min_size(egui::vec2(120.0, 32.0)))
                            .clicked()
                        {
                            let my_ip = local_ip();
                            self.handle.spawn(async move {
                                let (my_device_id, my_name, my_pub_key, my_port) = {
                                    let s = settings.read().await;
                                    (s.device_id, s.device_name.clone(), s.public_key.clone(), s.local_sync_port)
                                };

                                // Store Device A as trusted peer (using the token A generated)
                                let peer_a = crate::models::PeerDevice {
                                    id: req2.request.from_device_id,
                                    name: req2.request.from_name.clone(),
                                    public_key: req2.request.from_public_key.clone(),
                                    ip: req2.request.ip.clone(),
                                    port: req2.request.port,
                                    session_token: req2.request.session_token.clone(),
                                    paired_at: chrono::Utc::now(),
                                    online: false,
                                };

                                // Notify A — echo back the shared session_token
                                let resp = LanPairResponse {
                                    accepted: true,
                                    to_device_id: my_device_id,
                                    to_name: my_name,
                                    to_public_key: my_pub_key,
                                    ip: my_ip,
                                    port: my_port,
                                    session_token: req2.request.session_token.clone(),
                                };
                                match reqwest::Client::new()
                                    .post(&req2.respond_url)
                                    .json(&resp)
                                    .timeout(std::time::Duration::from_secs(5))
                                    .send()
                                    .await
                                {
                                    Ok(_) => { let _ = tx.try_send(Ok(peer_a)); }
                                    Err(e) => { let _ = tx.try_send(Err(format!("Accept failed: {e}"))); }
                                }
                            });
                        }

                        ui.add_space(8.0);

                        if ui.add(egui::Button::new("Reject")
                            .min_size(egui::vec2(100.0, 32.0)))
                            .clicked()
                        {
                            state.lock().unwrap().pending_pair_request = None;
                        }
                    });
                    ui.add_space(8.0);
                });
            });
    }

    fn render_pair_token_modal(&mut self, ctx: &egui::Context) {
        let info = match &self.pair_token_modal {
            Some(i) => i.clone(),
            None => return,
        };

        let now = chrono::Utc::now();
        let remaining = (info.expires_at - now).num_seconds().max(0);
        let expired = remaining == 0;

        egui::Window::new("Pair a new device")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .min_width(380.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);

                    // QR code
                    paint_qr(ui, &info.pair_uri, 220.0);
                    ui.add_space(8.0);

                    if expired {
                        ui.label(RichText::new("Expired").color(Color32::RED).strong());
                    } else {
                        let mins = remaining / 60;
                        let secs = remaining % 60;
                        ui.label(
                            RichText::new(format!("Expires in {mins:02}:{secs:02}"))
                                .color(Color32::from_rgb(180, 180, 100))
                                .small(),
                        );
                    }
                    ui.add_space(8.0);

                    // Server URL
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Server:").small().color(Color32::GRAY));
                        ui.label(RichText::new(&info.server_url).monospace().small());
                        if ui.small_button("Copy").clicked() {
                            ui.output_mut(|o| o.copied_text = info.server_url.clone());
                        }
                    });

                    // Token
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Token:").small().color(Color32::GRAY));
                        let short = format!("{}…", &info.token[..16.min(info.token.len())]);
                        ui.label(RichText::new(short).monospace().small());
                        if ui.small_button("Copy").clicked() {
                            ui.output_mut(|o| o.copied_text = info.token.clone());
                        }
                    });

                    // Pair URI
                    ui.add_space(6.0);
                    ui.label(RichText::new("Pair URI (paste into the new device):").small().color(Color32::GRAY));
                    egui::Frame::none()
                        .fill(Color32::from_rgb(25, 25, 30))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::same(6.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&info.pair_uri).monospace().small().color(Color32::from_rgb(150, 220, 150)));
                        });
                    if ui.button("Copy Pair URI").clicked() {
                        ui.output_mut(|o| o.copied_text = info.pair_uri.clone());
                    }

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            self.pair_token_modal = None;
                        }
                        if !expired && ui.button("Regenerate").clicked() {
                            self.pair_token_modal = None;
                            self.request_pair_token();
                        }
                    });
                    ui.add_space(8.0);
                });
            });
    }

    fn render_menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Clear History").clicked() {
                        self.state.lock().unwrap().clips.clear();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit ClipX").clicked() {
                        std::process::exit(0);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Settings…").clicked() {
                        self.screen = Screen::Settings;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About ClipX").clicked() {
                        self.screen = Screen::Settings;
                        ui.close_menu();
                    }
                });
            });
        });
    }

    fn render_settings_screen(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("settings_back_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back").clicked() {
                    self.screen = Screen::Main;
                }
                ui.separator();
                ui.label(RichText::new("Settings").strong());
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_settings_content(ui);
        });
    }

    fn render_settings_content(&mut self, ui: &mut egui::Ui) {
        // Snapshot the current settings once; the form edits a local copy, and
        // we persist on change via update_settings(). Falls back to defaults if
        // the lock is contended (rare with try_read).
        let snap: Settings = self
            .settings
            .try_read()
            .map(|s| s.clone())
            .unwrap_or_default();

        let mut device_name = snap.device_name.clone();
        let mut hotkey = snap.overlay_hotkey.clone();
        let mut history_limit = snap.history_limit;
        let mut max_retries = snap.max_cloud_retries;
        let mut e2e_enabled = snap.e2e_enabled;
        let mut auto_paste = snap.auto_paste_inbound;
        let mut lan_enabled = snap.lan_enabled;
        let mut local_port = snap.local_sync_port;

        ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);

            // ── General ──────────────────────────────────────────────────────
            ui.label(RichText::new("General").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("settings_general")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Device name");
                    if ui.add(egui::TextEdit::singleline(&mut device_name).desired_width(220.0)).lost_focus()
                        && device_name != snap.device_name
                        && !device_name.trim().is_empty()
                    {
                        let new_name = device_name.clone();
                        self.update_settings(move |s| s.device_name = new_name);
                    }
                    ui.end_row();

                    ui.label("Device ID");
                    ui.label(RichText::new(format!("{}…", &snap.device_id.to_string()[..8])).monospace().color(Color32::GRAY));
                    ui.end_row();

                    ui.label("Overlay hotkey");
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut hotkey).desired_width(160.0)).lost_focus()
                            && hotkey != snap.overlay_hotkey
                        {
                            let new_hk = hotkey.clone();
                            self.update_settings(move |s| s.overlay_hotkey = new_hk);
                        }
                        ui.label(RichText::new("(restart to apply)").color(Color32::GRAY).small());
                    });
                    ui.end_row();

                    ui.label("Start on login");
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, "(coming soon)"));
                    ui.end_row();

                    ui.label("Theme");
                    ui.label(RichText::new("Dark (coming soon)").color(Color32::GRAY));
                    ui.end_row();
                });

            ui.add_space(16.0);

            // ── Clipboard ────────────────────────────────────────────────────
            ui.label(RichText::new("Clipboard").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("settings_clipboard")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("History limit");
                    if ui.add(egui::Slider::new(&mut history_limit, 10..=500).text("items")).changed()
                        && history_limit != snap.history_limit
                    {
                        let new_limit = history_limit;
                        let state = self.state.clone();
                        self.update_settings(move |s| s.history_limit = new_limit);
                        // Apply to the in-memory deque immediately.
                        let mut s = state.lock().unwrap();
                        s.history_limit = new_limit as usize;
                        s.enforce_history_limit();
                    }
                    ui.end_row();

                    ui.label("Auto-paste inbound");
                    if ui.checkbox(&mut auto_paste, "Write received clips to OS clipboard").changed()
                        && auto_paste != snap.auto_paste_inbound
                    {
                        self.update_settings(move |s| s.auto_paste_inbound = auto_paste);
                    }
                    ui.end_row();

                    ui.label("Auto-clear on lock");
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, "(coming soon)"));
                    ui.end_row();
                });

            ui.add_space(16.0);

            // ── Excluded Apps ────────────────────────────────────────────────
            ui.label(RichText::new("Excluded Apps").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            ui.label(
                RichText::new("Clips copied from these apps will be silently ignored.")
                    .color(Color32::GRAY)
                    .small(),
            );
            ui.add_space(4.0);

            let excluded_apps = snap.excluded_apps.clone();

            // Build the app list: seen apps (starred) + running GUI apps + well-known suggestions
            let seen_apps: std::collections::BTreeSet<String> =
                self.state.lock().unwrap().seen_apps.clone();
            let running_apps = get_running_gui_apps();
            let suggestions: &[&str] = &["keepass.exe", "1password.exe", "bitwarden.exe", "lastpass.exe"];

            let mut all_apps: Vec<(String, bool)> = Vec::new(); // (name, is_seen)
            for app in &seen_apps {
                all_apps.push((app.clone(), true));
            }
            for app in &running_apps {
                if !all_apps.iter().any(|a| a.0 == *app) {
                    all_apps.push((app.clone(), false));
                }
            }
            for app in suggestions {
                if !all_apps.iter().any(|a| a.0 == *app) {
                    all_apps.push((app.to_string(), false));
                }
            }
            all_apps.retain(|a| !excluded_apps.iter().any(|e| e.to_lowercase() == a.0.to_lowercase()));
            all_apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            // Search + select
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.excluded_app_input)
                        .desired_width(220.0)
                        .hint_text("Search or type app name\u{2026}"),
                );
                let can_add = !self.excluded_app_input.trim().is_empty();
                if ui.add_enabled(can_add, egui::Button::new("Exclude")).clicked() && can_add {
                    let mut name = self.excluded_app_input.trim().to_lowercase();
                    if !name.ends_with(".exe") {
                        name.push_str(".exe");
                    }
                    let add_name = name.clone();
                    self.update_settings(move |s| {
                        if !s.excluded_apps.iter().any(|a| a.to_lowercase() == add_name) {
                            s.excluded_apps.push(add_name);
                        }
                    });
                    self.excluded_app_input.clear();
                }
            });
            ui.add_space(2.0);

            // Scrollable filtered app list
            let filter = self.excluded_app_input.to_lowercase();
            let filtered: Vec<&(String, bool)> = all_apps
                .iter()
                .filter(|(name, _)| filter.is_empty() || name.contains(&filter))
                .collect();

            let list_height = 120.0_f32.min(filtered.len() as f32 * 22.0 + 4.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("excluded_app_list")
                    .max_height(list_height)
                    .show(ui, |ui| {
                        if filtered.is_empty() {
                            ui.label(RichText::new("No matching apps").color(Color32::GRAY).small());
                        }
                        for (name, is_seen) in &filtered {
                            ui.horizontal(|ui| {
                                let label = if *is_seen {
                                    format!("\u{2605} {name}")
                                } else {
                                    name.to_string()
                                };
                                if ui.selectable_label(false, &label).clicked() {
                                    self.excluded_app_input = name.clone();
                                }
                                if *is_seen {
                                    ui.label(RichText::new("used").color(Color32::from_rgb(255, 200, 60)).small());
                                }
                            });
                        }
                    });
            });
            ui.add_space(4.0);

            if excluded_apps.is_empty() {
                ui.label(RichText::new("No apps excluded.").color(Color32::GRAY).italics());
            } else {
                for app in &excluded_apps {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(app).monospace());
                        if ui.small_button("\u{2715}").clicked() {
                            let remove_app = app.to_lowercase();
                            self.update_settings(move |s| {
                                s.excluded_apps.retain(|a| a.to_lowercase() != remove_app);
                            });
                        }
                    });
                }
            }

            ui.add_space(16.0);

            // ── Cloud Sync ───────────────────────────────────────────────────
            ui.label(RichText::new("Cloud Sync").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("settings_cloud")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Max retry attempts");
                    if ui.add(egui::Slider::new(&mut max_retries, 1..=50)).changed()
                        && max_retries != snap.max_cloud_retries
                    {
                        self.update_settings(move |s| s.max_cloud_retries = max_retries);
                    }
                    ui.end_row();

                    ui.label("Retry backoff");
                    ui.label(RichText::new("exponential, 5 min after limit").color(Color32::GRAY));
                    ui.end_row();

                    ui.label("E2E encryption");
                    if ui.checkbox(&mut e2e_enabled, "Encrypt clips end-to-end").changed()
                        && e2e_enabled != snap.e2e_enabled
                    {
                        self.update_settings(move |s| s.e2e_enabled = e2e_enabled);
                    }
                    ui.end_row();
                });
            if e2e_enabled {
                ui.label(
                    RichText::new(
                        "Clips are encrypted with the recipient's X25519 public key. The server cannot read your clips. Recipients without a known public key are skipped.",
                    )
                    .color(Color32::GRAY)
                    .small(),
                );
            }

            ui.add_space(16.0);

            // ── LAN Sync ─────────────────────────────────────────────────────
            ui.label(RichText::new("LAN Sync").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("settings_lan")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("LAN sync enabled");
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut lan_enabled, "").changed()
                            && lan_enabled != snap.lan_enabled
                        {
                            self.update_settings(move |s| s.lan_enabled = lan_enabled);
                        }
                        ui.label(RichText::new("(restart to apply)").color(Color32::GRAY).small());
                    });
                    ui.end_row();

                    ui.label("Listen port");
                    ui.horizontal(|ui| {
                        if ui.add(egui::DragValue::new(&mut local_port).range(1024..=65535)).changed()
                            && local_port != snap.local_sync_port
                        {
                            self.update_settings(move |s| s.local_sync_port = local_port);
                        }
                        ui.label(RichText::new("(restart to apply)").color(Color32::GRAY).small());
                    });
                    ui.end_row();
                });

            ui.add_space(16.0);

            // ── About ────────────────────────────────────────────────────────
            ui.label(RichText::new("About").strong().size(14.0));
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("settings_about")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Version");
                    ui.label(RichText::new(env!("CARGO_PKG_VERSION")).monospace());
                    ui.end_row();

                    ui.label("Build");
                    ui.label(RichText::new("debug").monospace().color(Color32::GRAY));
                    ui.end_row();
                });
        });
    }

    fn render_log(&self, ui: &mut egui::Ui) {
        let s = self.state.lock().unwrap();

        if s.log.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No events yet").color(Color32::GRAY));
            });
            return;
        }

        ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for entry in s.log.iter() {
                let color = match entry.level {
                    LogLevel::Info => Color32::from_rgb(200, 200, 200),
                    LogLevel::Warn => Color32::YELLOW,
                    LogLevel::Error => Color32::RED,
                    LogLevel::Success => Color32::GREEN,
                };
                let time_str = entry.time.format("%H:%M:%S%.3f").to_string();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&time_str).color(Color32::GRAY).monospace().small());
                    ui.label(RichText::new(&entry.message).color(color).monospace().small());
                });
            }
        });
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let radius = 7.0_f32;
    let size = egui::vec2(radius * 2.0 + 4.0, radius * 2.0 + 4.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();

    painter.circle_filled(center + egui::vec2(1.0, 1.5), radius, Color32::from_black_alpha(60));
    painter.circle_filled(center, radius, color);
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0, darken(color, 0.55)));
    painter.circle_filled(center + egui::vec2(-2.5, -2.5), radius * 0.35, Color32::from_white_alpha(160));
}

fn darken(c: Color32, factor: f32) -> Color32 {
    Color32::from_rgba_premultiplied(
        (c.r() as f32 * factor) as u8,
        (c.g() as f32 * factor) as u8,
        (c.b() as f32 * factor) as u8,
        c.a(),
    )
}

fn parse_ip_port(input: &str, default_port: u16) -> Result<(String, u16), String> {
    let (ip_str, port) = match input.rsplit_once(':') {
        Some((ip, p)) => {
            let port: u16 = p.parse().map_err(|_| format!("bad port: {p}"))?;
            (ip, port)
        }
        None => (input, default_port),
    };
    let ip_str = ip_str.trim_matches(|c| c == '[' || c == ']');
    let _: std::net::IpAddr = ip_str.parse().map_err(|_| format!("bad IP: {ip_str}"))?;
    Ok((ip_str.to_string(), port))
}

fn local_ip() -> String {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok();
    if let Some(s) = socket {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".to_string()
}

#[cfg(windows)]
fn capture_foreground_hwnd() -> Option<isize> {
    unsafe extern "system" {
        fn GetForegroundWindow() -> *mut std::ffi::c_void;
    }
    let h = unsafe { GetForegroundWindow() };
    if h.is_null() { None } else { Some(h as isize) }
}

#[cfg(not(windows))]
fn capture_foreground_hwnd() -> Option<isize> { None }

#[cfg(windows)]
fn restore_foreground(hwnd: isize) {
    unsafe extern "system" {
        fn SetForegroundWindow(hwnd: *mut std::ffi::c_void) -> i32;
        fn ShowWindow(hwnd: *mut std::ffi::c_void, n_cmd_show: i32) -> i32;
    }
    const SW_RESTORE: i32 = 9;
    let h = hwnd as *mut std::ffi::c_void;
    unsafe {
        ShowWindow(h, SW_RESTORE);
        SetForegroundWindow(h);
    }
}

#[cfg(not(windows))]
fn restore_foreground(_hwnd: isize) {}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn paint_qr(ui: &mut egui::Ui, data: &str, target_size: f32) {
    use qrcode::QrCode;
    let code = match QrCode::new(data.as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            ui.label(RichText::new("QR encode failed").color(Color32::RED).small());
            return;
        }
    };
    let modules: Vec<Vec<bool>> = (0..code.width())
        .map(|y| (0..code.width()).map(|x| code[(x, y)] == qrcode::Color::Dark).collect())
        .collect();
    let n = modules.len();
    let cell = (target_size / n as f32).floor().max(2.0);
    let total = cell * n as f32;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(total + 16.0, total + 16.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    // White background with 8px quiet zone
    painter.rect_filled(rect, 4.0, Color32::WHITE);
    let origin = rect.min + egui::vec2(8.0, 8.0);
    for (y, row) in modules.iter().enumerate() {
        for (x, &dark) in row.iter().enumerate() {
            if dark {
                let p = origin + egui::vec2(x as f32 * cell, y as f32 * cell);
                painter.rect_filled(
                    egui::Rect::from_min_size(p, egui::vec2(cell, cell)),
                    0.0,
                    Color32::BLACK,
                );
            }
        }
    }
}

#[cfg(windows)]
fn get_running_gui_apps() -> Vec<String> {
    use std::collections::BTreeSet;
    use std::ffi::c_void;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct EnumData {
        pids: Vec<u32>,
    }

    unsafe extern "system" fn enum_callback(hwnd: *mut c_void, lparam: isize) -> i32 {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn IsWindowVisible(hWnd: *mut c_void) -> i32;
            fn GetWindowThreadProcessId(hWnd: *mut c_void, lpdwProcessId: *mut u32) -> u32;
        }

        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != 0 {
            let data = unsafe { &mut *(lparam as *mut EnumData) };
            data.pids.push(pid);
        }
        1
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(lpEnumFunc: unsafe extern "system" fn(*mut c_void, isize) -> i32, lParam: isize) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> *mut c_void;
        fn CloseHandle(hObject: *mut c_void) -> i32;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetModuleFileNameExW(hProcess: *mut c_void, hModule: *mut c_void, lpFilename: *mut u16, nSize: u32) -> u32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    let mut data = EnumData { pids: Vec::new() };
    unsafe { EnumWindows(enum_callback, &mut data as *mut EnumData as isize) };

    let mut apps = BTreeSet::new();
    for pid in data.pids {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            continue;
        }
        let mut buf = [0u16; 1024];
        let len = unsafe { GetModuleFileNameExW(handle, std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
        unsafe { CloseHandle(handle) };
        if len > 0 {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            if let Some(name) = path.rsplit('\\').next() {
                let lower = name.to_lowercase();
                if lower.ends_with(".exe") && lower != "explorer.exe" {
                    apps.insert(lower);
                }
            }
        }
    }
    apps.into_iter().collect()
}

#[cfg(not(windows))]
fn get_running_gui_apps() -> Vec<String> {
    Vec::new()
}
