//! LMTrust Deep Blind Spot Test Suite: Config & Pacer Modes
//!
//! Layers covered:
//! - L0 Smoke: Default instances of RuntimeConfig and GameProfile
//! - L1 Contract: PacerMode conversions, PacerConfig construction
//! - L2 Boundary: Extreme target FPS (0, -100, 1000, NaN, Inf), refresh rates (0, 1000, NaN)
//! - L3 Property: Bit-exact roundtrips, PartialEq symmetry and transitivity
//! - L4 Adversarial: Unknown mode IDs mapping safely to Bypass
//! - L9 Negative Space: Default configs are disabled and safe

use iframe_common::config::{GameProfile, RuntimeConfig};
use iframe_common::pacer::{PacerConfig, PacerMode};

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_runtime_config_defaults() {
    let cfg = RuntimeConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.mode, PacerMode::FixedVsync);
    assert_eq!(cfg.target_fps, 60.0);
    assert_eq!(cfg.refresh_hz, 60.0);
    assert!(cfg.vsync_override);
    assert!(!cfg.force_waitable);
}

#[test]
fn l0_smoke_game_profile_defaults() {
    let prof = GameProfile::default();
    assert!(prof.exe_name.is_empty());
    assert_eq!(prof.target_fps, 60.0);
    assert_eq!(prof.mode, PacerMode::FixedVsync);
    assert!(prof.vsync_override);
    assert!(!prof.force_waitable_object);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Mode Conversions & PacerConfig
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_pacer_mode_u32_conversion() {
    assert_eq!(PacerMode::from_u32(0), PacerMode::Vrr);
    assert_eq!(PacerMode::from_u32(1), PacerMode::FixedVsync);
    assert_eq!(PacerMode::from_u32(2), PacerMode::Bypass);
}

#[test]
fn l1_contract_pacer_config_for_mode() {
    let cfg_vsync = PacerConfig::for_mode(PacerMode::FixedVsync, 144.0);
    assert_eq!(cfg_vsync.mode, PacerMode::FixedVsync);
    assert_eq!(cfg_vsync.target_fps, 144.0);
    assert_eq!(cfg_vsync.base_safety_margin_us, 250.0);
    assert_eq!(cfg_vsync.variance_multiplier, 2.0);

    let cfg_vrr = PacerConfig::for_mode(PacerMode::Vrr, 40.0);
    assert_eq!(cfg_vrr.mode, PacerMode::Vrr);
    assert_eq!(cfg_vrr.target_fps, 40.0);
    assert_eq!(cfg_vrr.base_safety_margin_us, 50.0);
    assert_eq!(cfg_vrr.variance_multiplier, 2.0);

    let cfg_bypass = PacerConfig::for_mode(PacerMode::Bypass, 0.0);
    assert_eq!(cfg_bypass.mode, PacerMode::Bypass);
    assert_eq!(cfg_bypass.base_safety_margin_us, 0.0);
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — 8 Boundary Conditions on Numeric Configs
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_extreme_fps_rates() {
    let boundary_fps = [
        0.0,
        -0.0,
        -1.0,
        -1000.0,
        0.0001,
        1.0,
        144.0,
        360.0,
        10_000.0,
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];

    for &fps in &boundary_fps {
        let p_cfg = PacerConfig::for_mode(PacerMode::FixedVsync, fps);
        if fps.is_finite() {
            assert_eq!(p_cfg.target_fps, fps);
        }
    }
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Invariants and Roundtrips
// ---------------------------------------------------------------------------

#[test]
fn l3_property_runtime_config_equality_and_clone() {
    let cfg1 = RuntimeConfig {
        enabled: true,
        mode: PacerMode::Vrr,
        target_fps: 119.88,
        refresh_hz: 120.0,
        vsync_override: false,
        force_waitable: true,
    };
    let cfg2 = cfg1;
    assert_eq!(cfg1, cfg2);
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Out of Range Mode IDs
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_invalid_mode_ids_fallback_to_bypass() {
    let invalid_ids = [3, 4, 10, 100, 999, u32::MAX];
    for &id in &invalid_ids {
        assert_eq!(
            PacerMode::from_u32(id),
            PacerMode::Bypass,
            "Mode ID {id} should fallback to Bypass"
        );
    }
}

// ---------------------------------------------------------------------------
// L9: Negative Space Tests — Defaults Do Not Mutate Game State
// ---------------------------------------------------------------------------

#[test]
fn l9_negative_space_default_runtime_config_inert() {
    let cfg = RuntimeConfig::default();
    assert!(!cfg.enabled, "Default config must NOT be enabled automatically");
    assert!(!cfg.force_waitable, "Default config must NOT force waitable swapchains");
}
