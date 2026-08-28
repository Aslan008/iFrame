//! Runtime configuration exchanged through the shared-memory header
//! (app → DLL), plus per-game profiles persisted by the app (M3).

use crate::pacer::PacerMode;

/// Live settings the control app publishes to the injected hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeConfig {
    pub enabled: bool,
    pub mode: PacerMode,
    pub target_fps: f64,
    /// Display refresh rate as reported/overridden by the user.
    pub refresh_hz: f64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: PacerMode::FixedVsync,
            target_fps: 60.0,
            refresh_hz: 60.0,
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
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            exe_name: String::new(),
            target_fps: 60.0,
            mode: PacerMode::FixedVsync,
            vsync_override: true,
            force_waitable_object: false,
        }
    }
}