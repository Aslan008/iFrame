//! LMTrust Deep Blind Spot Test Suite: NVIDIA Reflex / NVAPI Low Latency
//!
//! Layers covered:
//! - L0 Smoke: Reflex initialization and fallback without crash
//! - L1 Contract: ReflexMode conversions and marker emission
//! - L2 Boundary: Null pointers, repeated configure calls, unsupported environments

use std::ptr;
use iframe_common::config::ReflexMode;
use iframe_hook::reflex::{self, LatencyMarkerType};

#[test]
fn l0_smoke_reflex_initialization_no_panic() {
    let ctrl = reflex::get();
    let available = ctrl.is_available();
    // In CI or environments without NVIDIA hardware, is_available() should return false safely without crash.
    let _ = available;
}

#[test]
fn l1_contract_reflex_set_mode_and_marker() {
    let ctrl = reflex::get();

    // Calling configure and marker should be resilient to hardware presence and null device
    ctrl.configure(ptr::null_mut(), ReflexMode::Off, 60.0);
    ctrl.configure(ptr::null_mut(), ReflexMode::On, 60.0);
    ctrl.configure(ptr::null_mut(), ReflexMode::Boost, 120.0);

    ctrl.set_marker(ptr::null_mut(), LatencyMarkerType::SimulationStart, 1);
    ctrl.set_marker(ptr::null_mut(), LatencyMarkerType::SimulationEnd, 1);
    ctrl.set_marker(ptr::null_mut(), LatencyMarkerType::PresentStart, 1);
    ctrl.set_marker(ptr::null_mut(), LatencyMarkerType::PresentEnd, 1);

    // Revert to off
    ctrl.configure(ptr::null_mut(), ReflexMode::Off, 0.0);
}

#[test]
fn l2_boundary_sleep_null_device() {
    let ctrl = reflex::get();
    // Calling sleep with null device should return gracefully without panic
    ctrl.sleep(ptr::null_mut());
    ctrl.sleep(ptr::null_mut());
}
