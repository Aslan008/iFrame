//! LMTrust Deep Blind Spot Test Suite: Injector & Process Inspection
//!
//! Layers covered:
//! - L0 Smoke: default_dll_path_for resolution
//! - L1 Contract: is_wow64 returns Result<bool, String>
//! - L2 Boundary: Empty window titles, whitespace, unicode, 0 PID
//! - L4 Adversarial: Invalid PIDs (0, u32::MAX, random high PIDs)
//! - L9 Negative Space: Missing windows return Err without hanging or crashing

use iframe_app::injector::{default_dll_path_for, is_wow64, pid_from_window_title};

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_default_dll_path_resolution() {
    let current_pid = std::process::id();
    let dll_path = default_dll_path_for(current_pid);
    let path_str = dll_path.to_string_lossy().to_lowercase();
    assert!(
        path_str.ends_with("iframe_hook.dll") || path_str.ends_with("iframe_hook32.dll"),
        "Resolved DLL path must end with hook DLL name: {}",
        dll_path.display()
    );
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Process Architecture Detection
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_is_wow64_current_process() {
    let current_pid = std::process::id();
    let res = is_wow64(current_pid);
    assert!(res.is_ok(), "is_wow64 on current process must succeed");
    // On 64-bit Rust test binary on 64-bit Windows, is_wow64 is false
    #[cfg(target_pointer_width = "64")]
    assert_eq!(res.unwrap(), false);
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — Empty and Special Window Titles
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_window_title_empty_and_special() {
    assert!(pid_from_window_title("").is_err());
    assert!(pid_from_window_title("   ").is_err());
    assert!(pid_from_window_title("\0").is_err());
    assert!(pid_from_window_title("NonExistentWindow_987654321_XYZZY").is_err());
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Invalid and System PIDs
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_is_wow64_invalid_pids() {
    let invalid_pids = [0u32, 4_000_000_000u32, u32::MAX];
    for &pid in &invalid_pids {
        assert!(
            is_wow64(pid).is_err(),
            "Invalid PID {pid} must return Err instead of panicking"
        );
    }
}

// ---------------------------------------------------------------------------
// L9: Negative Space Tests — Non-Existent Windows Do Not Return Garbage PID
// ---------------------------------------------------------------------------

#[test]
fn l9_negative_space_missing_window_returns_clean_err() {
    let res = pid_from_window_title("Definitely_Not_A_Real_Window_Title_ABC_123");
    assert!(res.is_err());
    let err_msg = res.unwrap_err();
    assert!(!err_msg.is_empty());
}
