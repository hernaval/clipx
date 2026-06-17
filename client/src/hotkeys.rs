use std::collections::HashMap;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub enum HotkeyAction {
    ToggleOverlay,
    PasteSlot(u8),
}

pub struct HotkeyManager {
    #[allow(dead_code)]
    manager: GlobalHotKeyManager,
    #[allow(dead_code)]
    overlay_hotkey_id: u32,
    hotkey_map: HashMap<u32, HotkeyAction>,
    #[allow(dead_code)]
    registered_hotkeys: HashMap<u32, HotKey>,
}

impl HotkeyManager {
    pub fn new(hotkey_str: &str) -> anyhow::Result<Self> {
        let manager = GlobalHotKeyManager::new()?;
        let mut hotkey_map = HashMap::new();
        let mut registered_hotkeys = HashMap::new();

        let overlay = parse_hotkey(hotkey_str)?;
        let overlay_id = overlay.id();
        manager.register(overlay)?;
        hotkey_map.insert(overlay_id, HotkeyAction::ToggleOverlay);
        registered_hotkeys.insert(overlay_id, parse_hotkey(hotkey_str)?);
        info!("Registered overlay hotkey: {hotkey_str}");

        for digit in 1u8..=9 {
            let slot_str = format!("Ctrl+{digit}");
            match parse_hotkey(&slot_str) {
                Ok(hk) => {
                    let id = hk.id();
                    if let Err(e) = manager.register(hk) {
                        warn!("Failed to register {slot_str}: {e}");
                        continue;
                    }
                    hotkey_map.insert(id, HotkeyAction::PasteSlot(digit));
                    registered_hotkeys.insert(id, parse_hotkey(&slot_str).unwrap());
                    info!("Registered slot hotkey: {slot_str}");
                }
                Err(e) => warn!("Failed to parse {slot_str}: {e}"),
            }
        }

        Ok(Self { manager, overlay_hotkey_id: overlay_id, hotkey_map, registered_hotkeys })
    }

    #[allow(dead_code)]
    pub fn update_hotkey(&mut self, hotkey_str: &str) -> anyhow::Result<()> {
        if let Some(old_hk) = self.registered_hotkeys.remove(&self.overlay_hotkey_id) {
            let _ = self.manager.unregister(old_hk);
        }
        self.hotkey_map.remove(&self.overlay_hotkey_id);

        let new_hotkey = parse_hotkey(hotkey_str)?;
        let new_id = new_hotkey.id();
        self.manager.register(new_hotkey)?;
        self.overlay_hotkey_id = new_id;
        self.hotkey_map.insert(new_id, HotkeyAction::ToggleOverlay);
        self.registered_hotkeys.insert(new_id, parse_hotkey(hotkey_str)?);
        info!("Updated hotkey to: {hotkey_str}");
        Ok(())
    }

    #[allow(dead_code)]
    pub fn hotkey_id(&self) -> u32 {
        self.overlay_hotkey_id
    }

    #[allow(dead_code)]
    pub fn resolve(&self, hotkey_id: u32) -> Option<&HotkeyAction> {
        self.hotkey_map.get(&hotkey_id)
    }

    pub fn hotkey_map(&self) -> &HashMap<u32, HotkeyAction> {
        &self.hotkey_map
    }
}

fn parse_hotkey(s: &str) -> anyhow::Result<HotKey> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let mut modifiers = Modifiers::empty();
    let mut key_code = None;

    for part in &parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" => modifiers |= Modifiers::ALT,
            "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            k => {
                key_code = Some(match k {
                    "a" => Code::KeyA,
                    "b" => Code::KeyB,
                    "c" => Code::KeyC,
                    "d" => Code::KeyD,
                    "e" => Code::KeyE,
                    "f" => Code::KeyF,
                    "g" => Code::KeyG,
                    "h" => Code::KeyH,
                    "i" => Code::KeyI,
                    "j" => Code::KeyJ,
                    "k" => Code::KeyK,
                    "l" => Code::KeyL,
                    "m" => Code::KeyM,
                    "n" => Code::KeyN,
                    "o" => Code::KeyO,
                    "p" => Code::KeyP,
                    "q" => Code::KeyQ,
                    "r" => Code::KeyR,
                    "s" => Code::KeyS,
                    "t" => Code::KeyT,
                    "u" => Code::KeyU,
                    "v" => Code::KeyV,
                    "w" => Code::KeyW,
                    "x" => Code::KeyX,
                    "y" => Code::KeyY,
                    "z" => Code::KeyZ,
                    "0" => Code::Digit0,
                    "1" => Code::Digit1,
                    "2" => Code::Digit2,
                    "3" => Code::Digit3,
                    "4" => Code::Digit4,
                    "5" => Code::Digit5,
                    "6" => Code::Digit6,
                    "7" => Code::Digit7,
                    "8" => Code::Digit8,
                    "9" => Code::Digit9,
                    "f1" => Code::F1,
                    "f2" => Code::F2,
                    "f3" => Code::F3,
                    "f4" => Code::F4,
                    "f5" => Code::F5,
                    "f6" => Code::F6,
                    "f7" => Code::F7,
                    "f8" => Code::F8,
                    "f9" => Code::F9,
                    "f10" => Code::F10,
                    "f11" => Code::F11,
                    "f12" => Code::F12,
                    "space" => Code::Space,
                    "tab" => Code::Tab,
                    "escape" | "esc" => Code::Escape,
                    _ => anyhow::bail!("Unknown key: {k}"),
                });
            }
        }
    }

    let code = key_code.ok_or_else(|| anyhow::anyhow!("No key specified in hotkey string"))?;
    Ok(HotKey::new(Some(modifiers), code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey() {
        let hk = parse_hotkey("Ctrl+Shift+V").unwrap();
        assert!(hk.id() > 0);
    }

    #[test]
    fn test_parse_hotkey_with_function_key() {
        let hk = parse_hotkey("Alt+F12").unwrap();
        assert!(hk.id() > 0);
    }

    #[test]
    fn test_parse_invalid_hotkey() {
        let result = parse_hotkey("Ctrl+InvalidKey");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_slot_hotkeys() {
        for digit in 1..=9 {
            let hk = parse_hotkey(&format!("Ctrl+{digit}")).unwrap();
            assert!(hk.id() > 0);
        }
    }
}
