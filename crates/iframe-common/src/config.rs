//! Runtime configuration exchanged through the shared-memory header
//! (app → DLL), plus per-game profiles persisted by the app (M3).

use crate::pacer::PacerMode;

/// NVIDIA Reflex latency mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum ReflexMode {
    #[default]
    Off = 0,
    On = 1,
    Boost = 2,
}

impl ReflexMode {
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::On,
            2 => Self::Boost,
            _ => Self::Off,
        }
    }

    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Live settings the control app publishes to the injected hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeConfig {
    pub enabled: bool,
    pub mode: PacerMode,
    pub target_fps: f64,
    /// Display refresh rate as reported/overridden by the user.
    pub refresh_hz: f64,
    /// К1: while pacing, override the game's VSync (`SyncInterval=0`, tearing
    /// when the swap chain supports it). Off = pass the game's present args
    /// through untouched. Mirrors the UI "override VSync" checkbox.
    pub vsync_override: bool,
    /// Per-game opt-in (M4): force `FRAME_LATENCY_WAITABLE_OBJECT` on new
    /// swap chains of this process (hook side, see dxgi.rs).
    pub force_waitable: bool,
    /// NVIDIA Reflex mode (Off = 0, On = 1, Boost = 2).
    pub reflex_mode: ReflexMode,
    /// In-game DXGI/D3D11 overlay HUD toggle.
    pub overlay_enabled: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: PacerMode::FixedVsync,
            target_fps: 60.0,
            refresh_hz: 60.0,
            vsync_override: true,
            force_waitable: false,
            reflex_mode: ReflexMode::Off,
            overlay_enabled: false,
        }
    }
}

/// Persisted per-game profile (keyed by executable name).
#[derive(Debug, Clone)]
pub struct GameProfile {
    pub exe_name: String,
    pub target_fps: f64,
    pub mode: PacerMode,
    /// Override the game's VSync (`SyncInterval=0`) while pacing — only for
    /// swapchains that support it (see plan correction К1).
    pub vsync_override: bool,
    /// Force `FRAME_LATENCY_WAITABLE_OBJECT` + `MaxFrameLatency(1)` at swap
    /// chain creation (M4, per-game opt-in, default off).
    pub force_waitable_object: bool,
    pub reflex_mode: ReflexMode,
    pub overlay_enabled: bool,
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            exe_name: String::new(),
            target_fps: 60.0,
            mode: PacerMode::FixedVsync,
            vsync_override: true,
            force_waitable_object: false,
            reflex_mode: ReflexMode::Off,
            overlay_enabled: false,
        }
    }
}