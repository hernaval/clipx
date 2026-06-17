use crate::models::Clip;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[allow(dead_code)]
pub struct OverlayState {
    pub visible: bool,
    pub clips: Vec<Clip>,
    pub selected_index: usize,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            clips: Vec::new(),
            selected_index: 0,
        }
    }
}

#[allow(dead_code)]
pub struct OverlayManager {
    state: Arc<RwLock<OverlayState>>,
}

#[allow(dead_code)]
impl OverlayManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(OverlayState::default())),
        }
    }

    pub async fn toggle(&self, clips: Vec<Clip>) {
        let mut state = self.state.write().await;
        state.visible = !state.visible;
        if state.visible {
            state.clips = clips;
            state.selected_index = 0;
            info!("Overlay shown with {} clips", state.clips.len());
        } else {
            info!("Overlay hidden");
        }
    }

    pub async fn select_up(&self) {
        let mut state = self.state.write().await;
        if state.selected_index > 0 {
            state.selected_index -= 1;
        }
    }

    pub async fn select_down(&self) {
        let mut state = self.state.write().await;
        if state.selected_index + 1 < state.clips.len() {
            state.selected_index += 1;
        }
    }

    pub async fn confirm_selection(&self) -> Option<String> {
        let mut state = self.state.write().await;
        if !state.visible || state.clips.is_empty() {
            return None;
        }
        let clip = &state.clips[state.selected_index];
        let content = clip.content.clone();
        state.visible = false;

        let content_for_ipc = content.clone();
        tokio::spawn(async move {
            let _ = crate::ipc::IpcClient::send_rpc(
                "set_clipboard",
                Some(serde_json::json!({ "content": content_for_ipc })),
            ).await;
        });

        info!("Selected clip: {}...", &content[..content.len().min(50)]);
        Some(content)
    }

    pub async fn is_visible(&self) -> bool {
        self.state.read().await.visible
    }

    pub async fn get_state(&self) -> (Vec<Clip>, usize, bool) {
        let state = self.state.read().await;
        (state.clips.clone(), state.selected_index, state.visible)
    }

    pub fn state_handle(&self) -> Arc<RwLock<OverlayState>> {
        self.state.clone()
    }
}

pub fn simulate_paste() {
    #[cfg(windows)]
    {
        use std::mem;

        #[repr(C)]
        #[allow(non_snake_case)]
        struct INPUT {
            type_: u32,
            union: INPUT_UNION,
        }

        #[repr(C)]
        #[derive(Copy, Clone)]
        #[allow(non_snake_case)]
        union INPUT_UNION {
            ki: KEYBDINPUT,
        }

        #[repr(C)]
        #[derive(Copy, Clone)]
        #[allow(non_snake_case)]
        struct KEYBDINPUT {
            wVk: u16,
            wScan: u16,
            dwFlags: u32,
            time: u32,
            dwExtraInfo: usize,
        }

        const INPUT_KEYBOARD: u32 = 1;
        const KEYEVENTF_KEYUP: u32 = 0x0002;
        const VK_CONTROL: u16 = 0x11;
        const VK_V: u16 = 0x56;

        unsafe extern "system" {
            fn SendInput(cInputs: u32, pInputs: *const INPUT, cbSize: i32) -> u32;
        }

        let inputs = [
            INPUT {
                type_: INPUT_KEYBOARD,
                union: INPUT_UNION {
                    ki: KEYBDINPUT {
                        wVk: VK_CONTROL,
                        wScan: 0,
                        dwFlags: 0,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                type_: INPUT_KEYBOARD,
                union: INPUT_UNION {
                    ki: KEYBDINPUT {
                        wVk: VK_V,
                        wScan: 0,
                        dwFlags: 0,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                type_: INPUT_KEYBOARD,
                union: INPUT_UNION {
                    ki: KEYBDINPUT {
                        wVk: VK_V,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                type_: INPUT_KEYBOARD,
                union: INPUT_UNION {
                    ki: KEYBDINPUT {
                        wVk: VK_CONTROL,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];

        unsafe {
            SendInput(4, inputs.as_ptr(), mem::size_of::<INPUT>() as i32);
        }
    }
}
