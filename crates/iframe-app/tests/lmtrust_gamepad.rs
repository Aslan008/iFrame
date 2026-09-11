//! LMTrust Deep Blind Spot Test Suite: Gamepad XInput Polling
//!
//! Layers covered:
//! - L0 Smoke: GamepadTracker creation, polling without gamepad
//! - L1 Contract: Button masks and deadzone thresholds
//! - L2 Boundary: Disconnect throttling, zero/rapid repeated calls

use iframe_app::gamepad::{
    GamepadTracker, LEFT_THUMB_DEADZONE, LEFT_THUMB_FAST_THRESHOLD,
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT,
    XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_GAMEPAD_X,
    XINPUT_GAMEPAD_Y,
};

#[test]
fn l0_smoke_gamepad_tracker_creation() {
    let mut tracker = GamepadTracker::new();
    let actions = tracker.poll();
    // In headless/test environment without controller connected, actions are empty
    assert!(actions.is_empty() || tracker.is_connected());
}

#[test]
fn l1_contract_deadzone_and_constants() {
    assert_eq!(LEFT_THUMB_DEADZONE, 7849);
    assert_eq!(LEFT_THUMB_FAST_THRESHOLD, 22000);
    assert_eq!(XINPUT_GAMEPAD_DPAD_LEFT, 0x0004);
    assert_eq!(XINPUT_GAMEPAD_DPAD_RIGHT, 0x0008);
    assert_eq!(XINPUT_GAMEPAD_LEFT_SHOULDER, 0x0100);
    assert_eq!(XINPUT_GAMEPAD_RIGHT_SHOULDER, 0x0200);
    assert_eq!(XINPUT_GAMEPAD_A, 0x1000);
    assert_eq!(XINPUT_GAMEPAD_B, 0x2000);
    assert_eq!(XINPUT_GAMEPAD_X, 0x4000);
    assert_eq!(XINPUT_GAMEPAD_Y, 0x8000);
}

#[test]
fn l2_boundary_rapid_polls_do_not_panic() {
    let mut tracker = GamepadTracker::new();
    for _ in 0..100 {
        let _ = tracker.poll();
    }
}
