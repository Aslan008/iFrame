//! In-game Direct3D HUD overlay module.
//!
//! Provides in-game frametime stats, current FPS, pacer cadence mode,
//! Reflex status, and hotkey toggle (`F11`).

pub mod d3d11;

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F11};

static OVERLAY_VISIBLE: AtomicBool = AtomicBool::new(true);
static KEY_PREV_DOWN: AtomicBool = AtomicBool::new(false);

/// Poll hotkey (F11) to toggle overlay visibility.
pub fn poll_hotkey() {
    unsafe {
        let state = GetAsyncKeyState(VK_F11.0 as i32);
        let is_down = (state as u16 & 0x8000) != 0;
        let was_down = KEY_PREV_DOWN.swap(is_down, Ordering::Relaxed);
        if is_down && !was_down {
            let cur = OVERLAY_VISIBLE.load(Ordering::Relaxed);
            OVERLAY_VISIBLE.store(!cur, Ordering::Relaxed);
        }
    }
}

pub fn is_visible() -> bool {
    OVERLAY_VISIBLE.load(Ordering::Relaxed)
}

pub fn set_visible(visible: bool) {
    OVERLAY_VISIBLE.store(visible, Ordering::Relaxed);
}
