use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::info;

#[allow(dead_code)]
pub enum TrayAction {
    ShowHistory,
    ToggleSync,
    Settings,
    Quit,
}

#[allow(dead_code)]
pub struct TrayManager {
    _tray: TrayIcon,
    pub show_history_id: muda::MenuId,
    pub toggle_sync_id: muda::MenuId,
    pub settings_id: muda::MenuId,
    pub quit_id: muda::MenuId,
}

#[allow(dead_code)]
impl TrayManager {
    pub fn new() -> anyhow::Result<Self> {
        let menu = Menu::new();

        let show_history = MenuItem::new("Show History", true, None);
        let toggle_sync = MenuItem::new("Toggle Sync", true, None);
        let settings = MenuItem::new("Settings", true, None);
        let quit = MenuItem::new("Quit", true, None);

        let show_history_id = show_history.id().clone();
        let toggle_sync_id = toggle_sync.id().clone();
        let settings_id = settings.id().clone();
        let quit_id = quit.id().clone();

        menu.append(&show_history)?;
        menu.append(&toggle_sync)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&settings)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let icon = create_default_icon();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ClipX - Clipboard Sync")
            .with_icon(icon)
            .build()?;

        info!("System tray initialized");

        Ok(Self {
            _tray: tray,
            show_history_id,
            toggle_sync_id,
            settings_id,
            quit_id,
        })
    }

    pub fn handle_event(&self, event: &MenuEvent) -> Option<TrayAction> {
        let id = &event.id;
        if *id == self.show_history_id {
            Some(TrayAction::ShowHistory)
        } else if *id == self.toggle_sync_id {
            Some(TrayAction::ToggleSync)
        } else if *id == self.settings_id {
            Some(TrayAction::Settings)
        } else if *id == self.quit_id {
            Some(TrayAction::Quit)
        } else {
            None
        }
    }
}

#[allow(dead_code)]
fn create_default_icon() -> Icon {
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let cx = (x as f32 - 16.0).abs();
            let cy = (y as f32 - 16.0).abs();
            if cx < 12.0 && cy < 12.0 {
                // Blue clipboard icon
                rgba.extend_from_slice(&[66, 133, 244, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("Failed to create icon")
}
