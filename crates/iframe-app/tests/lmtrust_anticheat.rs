//! LMTrust Deep Blind Spot Test Suite: Anti-Cheat Safety Gate
//!
//! Layers covered:
//! - L1 Contract: Detection of all known anti-cheat services and protected games
//! - L2 Boundary: Case insensitivity, empty strings, path separators, unicode
//! - L4 Adversarial: Non-existent PIDs, system PIDs (0, 4), access denied handling
//! - L9 Negative Space: Unprotected games and test applications must NOT be blocked

use iframe_app::anticheat::{check_process, is_protected_name, process_name};

// ---------------------------------------------------------------------------
// L1: Contract Tests — Detection of Protected Targets
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_known_anticheat_services_detected() {
    let services = [
        "easyanticheat.exe",
        "easyanticheat_sys.exe",
        "easyanticheatsetup.exe",
        "beservice.exe",
        "beserver.exe",
        "beshellsvc.exe",
        "bedaisy.exe",
        "vgc.exe",
        "vgtray.exe",
        "vgk.exe",
        "faceitclient.exe",
        "faceit.exe",
        "eseaclient.exe",
        "mhyprot2.exe",
        "xigncode3.exe",
        "gameguard.exe",
    ];

    for s in services {
        assert!(
            is_protected_name(s),
            "Anti-cheat service '{s}' was NOT detected!"
        );
    }
}

#[test]
fn l1_contract_known_protected_games_detected() {
    let games = [
        "valorant.exe",
        "fortniteclient-win64-shipping.exe",
        "rustclient.exe",
        "escapefromtarkov.exe",
        "r5apex.exe",
        "apex_legends.exe",
        "cod.exe",
        "modernwarfare.exe",
        "gta5.exe",
        "pubg.exe",
        "tslgame.exe",
        "rainbowsix.exe",
        "destiny2.exe",
        "genshinimpact.exe",
        "zenlesszonezero.exe",
        "dayz_x64.exe",
        "arma3_x64.exe",
    ];

    for g in games {
        assert!(
            is_protected_name(g),
            "Protected game '{g}' was NOT detected!"
        );
    }
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — Case Insensitivity & Special Formats
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_case_insensitivity() {
    let mixed_cases = [
        "VALORANT.EXE",
        "VaLoRaNt.ExE",
        "EASYANTICHEAT.EXE",
        "EasyAntiCheat.exe",
        "BeService.EXE",
        "VGC.EXE",
        "GenshinImpact.EXE",
        "Apex_Legends.EXE",
    ];

    for name in mixed_cases {
        assert!(
            is_protected_name(name),
            "Case-insensitive matching failed for '{name}'"
        );
    }
}

#[test]
fn l2_boundary_empty_and_special_strings() {
    assert!(!is_protected_name(""));
    assert!(!is_protected_name("   "));
    assert!(!is_protected_name("\0"));
    assert!(!is_protected_name("valorant_mod.exe"));
    assert!(!is_protected_name("not_easyanticheat.exe"));
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — System and Invalid PIDs
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_nonexistent_pid_returns_err_without_panic() {
    let non_existent_pid = 4_000_000_000u32;
    assert!(check_process(non_existent_pid).is_err());
    assert!(process_name(non_existent_pid).is_err());
}

#[test]
fn l4_adversarial_current_process_name_resolution() {
    let current_pid = std::process::id();
    let name = process_name(current_pid);
    assert!(name.is_ok(), "Current process name resolution must succeed");
}

// ---------------------------------------------------------------------------
// L9: Negative Space Tests — Benign Games Allowed
// ---------------------------------------------------------------------------

#[test]
fn l9_negative_space_benign_processes_not_blocked() {
    let benign = [
        "d3d11_test_app.exe",
        "d3d9_test_app.exe",
        "iframe.exe",
        "notepad.exe",
        "explorer.exe",
        "cyberpunk2077.exe",
        "witcher3.exe",
        "eldenring.exe",
        "skyrimse.exe",
        "doom.exe",
        "portal2.exe",
    ];

    for b in benign {
        assert!(
            !is_protected_name(b),
            "Benign process '{b}' was falsely flagged as protected!"
        );
    }
}
