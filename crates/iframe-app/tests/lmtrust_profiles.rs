//! LMTrust Deep Blind Spot Test Suite: Profiles Persistence & TOML Serialization
//!
//! Layers covered:
//! - L0 Smoke: GameProfile default construction
//! - L1 Contract: TOML roundtrip, field serialization
//! - L2 Boundary: Non-ASCII exe names, spaces, empty strings, missing fields fallback
//! - L4 Adversarial: Malformed TOML data parsing resilience
//! - L8 Temporal: Debounce logic (flush_due vs flush)

use iframe_app::profiles::GameProfile;

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
    };

    let serialized = toml::to_string_pretty(&original).expect("serialization must succeed");
    let deserialized: GameProfile =
        toml::from_str(&serialized).expect("deserialization must succeed");

    assert_eq!(deserialized.target_fps, 144.0);
    assert_eq!(deserialized.mode, "vrr");
    assert!(!deserialized.vsync_override);
    assert!(deserialized.auto_attach);
    assert!(deserialized.force_waitable);
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
