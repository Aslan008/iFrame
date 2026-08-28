//! System tray icon + global hotkey (Ctrl+Alt+I toggles the limiter).
//!
//! The tray is created on the main thread (inside the eframe/winit event
//! loop) so its hidden window's messages get pumped; events are delivered
//! through global channels that the UI polls each frame.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct Tray {
    pub show_item: MenuItem,
    pub quit_item: MenuItem,
    pub limiter_item: MenuItem,
    _tray: TrayIcon,
}

pub struct HotKeys {
    pub toggle_limiter: u32,
    _manager: GlobalHotKeyManager,
}

/// 32×32 RGBA icon: dark plate, green frame, green pace line.
fn icon_rgba() -> Vec<u8> {
    let (w, h) = (32usize, 32usize);
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let border = x < 3 || y < 3 || x >= w - 3 || y >= h - 3;
            // diagonal "pace line" from bottom-left to top-right
            let diag = (x as i32 - (h as i32 - 1 - y as i32) * 2).abs() < 5;
            let (r, g, b, a) = if border || diag {
                (0x30, 0xE0, 0x6A, 0xFF) // green
            } else {
                (0x10, 0x14, 0x18, 0xFF) // dark plate
            };
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = a;
        }
    }
    rgba
}

pub fn create_tray() -> Result<Tray, String> {
    let menu = Menu::new();
    let show_item = MenuItem::new("Show / Hide", true, None);
    let limiter_item = MenuItem::new("Toggle limiter (Ctrl+Alt+I)", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append_items(&[&show_item, &limiter_item, &quit_item])
        .map_err(|e| e.to_string())?;

    let icon = Icon::from_rgba(icon_rgba(), 32, 32).map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("iFrame — zero-lag frame pacer")
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;

    Ok(Tray {
        show_item,
        quit_item,
        limiter_item,
        _tray: tray,
    })
}

pub fn create_hotkeys() -> Result<HotKeys, String> {
    let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
    let hk = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyI);
    manager.register(hk).map_err(|e| e.to_string())?;
    Ok(HotKeys {
        toggle_limiter: hk.id(),
        _manager: manager,
    })
}

/// Drain pending tray menu events.
pub fn poll_menu_events() -> Vec<MenuEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
        out.push(ev);
    }
    out
}

/// Drain pending global hotkey presses (ids only, pressed state only).
pub fn poll_hotkey_events() -> Vec<u32> {
    let mut out = Vec::new();
    while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
        if ev.state == global_hotkey::HotKeyState::Pressed {
            out.push(ev.id);
        }
    }
    out
}