//! Configuration parameters for All In Frame Asynchronous Warp & Extrapolation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarpConfig {
    /// Master toggle for the warp engine.
    pub enabled: bool,
    /// Sensitivity multiplier for mouse yaw (horizontal camera rotation).
    pub yaw_sensitivity: f32,
    /// Sensitivity multiplier for mouse pitch (vertical camera rotation).
    pub pitch_sensitivity: f32,
    /// Camera horizontal field of view in degrees (default: 90.0).
    pub fov_degrees: f32,
    /// Near clipping plane distance in meters (default: 0.1).
    pub depth_near: f32,
    /// Far clipping plane distance in meters (default: 1000.0).
    pub depth_far: f32,
    /// Enable reverse-Z depth decoding (used by modern UE4/UE5 and Unity titles).
    pub is_reverse_z: bool,
    /// Enable automatic HUD/UI isolation by depth & motion mask.
    pub hud_mask_enabled: bool,
    /// Depth threshold for identifying 2D HUD elements (pixels with depth < threshold remain static).
    pub hud_depth_threshold: f32,
    /// Strength of bilateral edge inpainting to fill occluded crevices [0.0..1.0].
    pub inpainting_strength: f32,
    /// Frame Generation Multiplier (1 = 1x real-time warp only, 2 = 2x generated frames, 3 = 3x).
    pub fps_multiplier: u32,
    /// Continuous test wave oscillation (visual proof of warp without mouse input).
    pub continuous_test_wave: bool,
    /// Test pulse sequence counter.
    pub test_pulse: u32,
    /// Visual debug mode: render Z-buffer heatmap to verify depth capture.
    pub debug_depth: bool,
}

impl Default for WarpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            yaw_sensitivity: 0.0015,
            pitch_sensitivity: 0.0015,
            fov_degrees: 90.0,
            depth_near: 0.1,
            depth_far: 1000.0,
            is_reverse_z: false,
            hud_mask_enabled: false,
            hud_depth_threshold: 0.005,
            inpainting_strength: 0.8,
            fps_multiplier: 1,
            continuous_test_wave: false,
            test_pulse: 0,
            debug_depth: false,
        }
    }
}

impl WarpConfig {
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }
}
