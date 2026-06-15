use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use chrono::{DateTime, Local};
use uuid::Uuid;
use crate::models::{Clip, Device, InboundPairRequest, PeerDevice};

pub type SharedStateHandle = Arc<Mutex<SharedState>>;

pub struct SharedState {
    pub pipe_connected: bool,
    pub ws_connected: bool,
    pub clips: VecDeque<Clip>,
    pub log: VecDeque<LogEntry>,
    pub upload_count: u64,
    pub receive_count: u64,
    pub devices: Vec<Device>,
    pub devices_loading: bool,
    // LAN peer-to-peer state
    pub peer_devices: Vec<PeerDevice>,
    pub pending_pair_request: Option<InboundPairRequest>,
    pub lan_server_running: bool,
    // Cloud sync state
    pub cloud_sync_stopped: bool,
    // How many clips to retain in the in-memory history deque.
    pub history_limit: usize,
    // Last time `devices` was refreshed from the server. Used by sync.rs to
    // decide whether to re-fetch the recipient list before encrypting a clip.
    pub devices_fetched_at: Option<std::time::Instant>,
    pub seen_apps: BTreeSet<String>,
}

pub struct LogEntry {
    pub time: DateTime<Local>,
    pub level: LogLevel,
    pub message: String,
}

pub enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            pipe_connected: false,
            ws_connected: false,
            clips: VecDeque::new(),
            log: VecDeque::new(),
            upload_count: 0,
            receive_count: 0,
            devices: Vec::new(),
            devices_loading: false,
            peer_devices: Vec::new(),
            pending_pair_request: None,
            lan_server_running: false,
            cloud_sync_stopped: false,
            history_limit: 50,
            devices_fetched_at: None,
            seen_apps: BTreeSet::new(),
        }
    }

    /// Trim the in-memory clip deque to `history_limit`.
    pub fn enforce_history_limit(&mut self) {
        while self.clips.len() > self.history_limit {
            self.clips.pop_back();
        }
    }

    pub fn set_peer_online(&mut self, id: Uuid, online: bool) {
        if let Some(p) = self.peer_devices.iter_mut().find(|p| p.id == id) {
            p.online = online;
        }
    }

    pub fn log(&mut self, level: LogLevel, msg: impl Into<String>) {
        if self.log.len() >= 500 {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            time: Local::now(),
            level,
            message: msg.into(),
        });
    }

    pub fn add_clip(&mut self, clip: Clip) {
        self.clips.retain(|c| c.content != clip.content);
        while self.clips.len() >= self.history_limit {
            self.clips.pop_back();
        }
        self.clips.push_front(clip);
    }

    pub fn clip_for_slot(&self, slot: u8) -> Option<&Clip> {
        self.clips.iter().find(|c| c.pin_slot == Some(slot))
    }

    pub fn assign_slot(&mut self, clip_id: Uuid, slot: Option<u8>) {
        if let Some(s) = slot {
            for c in self.clips.iter_mut() {
                if c.pin_slot == Some(s) && c.id != clip_id {
                    c.pin_slot = None;
                }
            }
        }
        if let Some(c) = self.clips.iter_mut().find(|c| c.id == clip_id) {
            c.pin_slot = slot;
        }
    }
}
