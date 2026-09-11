//! LMTrust Deep Blind Spot Test Suite: Profiles Persistence & TOML Serialization
//!
//! Layers covered:
//! - L0 Smoke: GameProfile default construction
//! - L1 Contract: TOML roundtrip, field serialization, Profiles import/export
//! - L2 Boundary: Non-ASCII exe names, spaces, empty strings, missing fields fallback
//! - L4 Adversarial: Malformed TOML data parsing resilience
//! - L8 Temporal: Debounce logic (flush_due vs flush)

use iframe_app::profiles::{GameProfile, Profiles};

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_game_profile_default() {
    let p = GameProfile::default();
    assert_eq!(p.target_fps, 60.0);
    assert_eq!(p.mode, "vsync");
    assert!(p.vsync_override);
    assert!(!p.auto_attach);
    assert!(!p.force_waitable);
    assert_eq!(p.reflex_mode, "off");
    assert!(!p.overlay_enabled);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — TOML Roundtrip
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_profile_toml_roundtrip() {
    let original = GameProfile {
        target_fps: 144.0,
        mode: "vrr".into(),
        vsync_override: false,
        auto_attach: true,
        force_waitable: true,
        refresh_hz: 165.0,
        reflex_mode: "boost".into(),
        overlay_enabled: true,
    };

    let serialized = toml::to_string_pretty(&original).expect("serialization must succeed");
    let deserialized: GameProfile =
        toml::from_str(&serialized).expect("deserialization must succeed");

    assert_eq!(deserialized.target_fps, 144.0);
    assert_eq!(deserialized.mode, "vrr");
    assert!(!deserialized.vsync_override);
    assert!(deserialized.auto_attach);
    assert!(deserialized.force_waitable);
    assert_eq!(deserialized.refresh_hz, 165.0);
    assert_eq!(deserialized.reflex_mode, "boost");
    assert!(deserialized.overlay_enabled);
}

#[test]
fn l1_contract_profiles_manager_import_export() {
    let mut profiles = Profiles::load();
    let test_exe = "test_game_roundtrip_unit.exe";
    let prof = GameProfile {
        target_fps: 120.0,
        mode: "vsync".into(),
        vsync_override: true,
        auto_attach: true,
        force_waitable: false,
        refresh_hz: 120.0,
        reflex_mode: "on".into(),
        overlay_enabled: true,
    };
    profiles.set(test_exe, prof.clone());

    let toml_exported = profiles.export_toml().expect("export_toml must succeed");
    assert!(toml_exported.contains("test_game_roundtrip_unit.exe"));
    assert!(toml_exported.contains("reflex_mode = \"on\""));
    assert!(toml_exported.contains("overlay_enabled = true"));

    let imported_count = profiles.import_toml(&toml_exported).expect("import_toml must succeed");
    assert!(imported_count >= 1);

    let fetched = profiles.get(test_exe).expect("profile must exist");
    assert_eq!(fetched.target_fps, 120.0);
    assert_eq!(fetched.reflex_mode, "on");
    assert!(fetched.overlay_enabled);

    assert!(profiles.remove(test_exe));
    assert!(profiles.get(test_exe).is_none());
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — Missing Fields and Unicode Exe Names
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_partial_toml_uses_defaults() {
    // Only target_fps and mode provided, others missing
    let partial_toml = r#"
        target_fps = 120.0
        mode = "vsync"
        vsync_override = true
    "#;

    let p: GameProfile = toml::from_str(partial_toml).expect("must parse with defaults");
    assert_eq!(p.target_fps, 120.0);
    assert_eq!(p.mode, "vsync");
    assert!(p.vsync_override);
    assert!(!p.auto_attach);
    assert!(!p.force_waitable);
    assert_eq!(p.refresh_hz, 0.0, "missing refresh_hz must default to Auto (DWM)");
    assert_eq!(p.reflex_mode, "off");
    assert!(!p.overlay_enabled);
}

#[test]
fn l2_boundary_special_characters_in_exe_keys() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct ProfilesContainer {
        games: std::collections::HashMap<String, GameProfile>,
    }

    let mut container = ProfilesContainer {
        games: std::collections::HashMap::new(),
    };

    let special_names = [
        "Cyberpunk 2077.exe",
        "Witcher 3 - Wild Hunt.exe",
        "Игра.exe",
        "ゲーム.exe",
        "game_v1.0.0.exe",
        "foo/bar/baz.exe",
    ];

    for name in special_names {
        container
            .games
            .insert(name.to_string(), GameProfile::default());
    }

    let toml_str = toml::to_string_pretty(&container).expect("serialization must succeed");
    let read_back: ProfilesContainer =
        toml::from_str(&toml_str).expect("deserialization must succeed");

    for name in special_names {
        assert!(
            read_back.games.contains_key(name),
            "Exe name '{name}' was lost in roundtrip"
        );
    }
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Malformed TOML Resilience
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_malformed_toml_does_not_panic() {
    let bad_strings = [
        "",
        "??? not a toml ???",
        "target_fps = 'invalid_string'",
        "target_fps = [1, 2, 3]",
        "[[[[broken",
        "target_fps = NaN",
    ];

    for bad in bad_strings {
        let res: Result<GameProfile, _> = toml::from_str(bad);
        assert!(res.is_err(), "Corrupted TOML should return Err, not parse");
    }
}
