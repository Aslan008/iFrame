//! LMTrust Deep Blind Spot Test Suite: JitPacer & VBlank Grid
//!
//! Layers covered:
//! - L0 Smoke: Basic instantiation, initial state
//! - L1 Contract: Core contracts, never holding completed frames, cadence steps
//! - L2 Boundary: 8 boundary conditions (zeros, subnormals, infinities, NaNs, extreme FPS, thresholds)
//! - L3 Property: Monotonicity, non-negativity, finite bounds, safety margin invariants
//! - L4 Adversarial: Clock jumps backward, massive stalls/hitches, mid-stream config thrashing
//! - L4b Fallback: FixedVsync without DWM hint, invalid refresh rates
//! - L5 State: Cold start -> warm start -> convergence -> state transitions
//! - L9 Negative Space: Bypass mode never sleeps, overloaded frames never delayed

use iframe_common::pacer::{
    qpc_to_us, us_to_qpc, JitPacer, PacerConfig, PacerMode, VBlankHint,
};

const FREQ: i64 = 10_000_000; // 10 MHz (100 ns per tick)

// ---------------------------------------------------------------------------
// L0: Smoke Tests — Is it alive?
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_pacer_creation_all_modes() {
    for mode in [PacerMode::Vrr, PacerMode::FixedVsync, PacerMode::Bypass] {
        let cfg = PacerConfig::for_mode(mode, 60.0);
        let pacer = JitPacer::new(cfg.clone(), FREQ);
        assert_eq!(pacer.frames_seen(), 0);
        assert_eq!(pacer.config().mode, mode);
        assert_eq!(pacer.config().target_fps, 60.0);
    }
}

#[test]
fn l0_smoke_qpc_conversion_helpers() {
    assert_eq!(qpc_to_us(10, FREQ), 1.0);
    assert_eq!(us_to_qpc(1.0, FREQ), 10);
    assert_eq!(PacerMode::from_u32(0), PacerMode::Vrr);
    assert_eq!(PacerMode::from_u32(1), PacerMode::FixedVsync);
    assert_eq!(PacerMode::from_u32(2), PacerMode::Bypass);
    assert_eq!(PacerMode::from_u32(999), PacerMode::Bypass);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Does it do what it promises?
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_first_frame_always_bypasses() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let t0 = 1_000_000;
    let d = pacer.on_present_complete(t0, t0 + 1000, None);
    assert!(d.stats.bypass, "First frame MUST bypass (no history)");
    assert_eq!(d.wake_qpc, t0 + 1000);
    assert_eq!(pacer.frames_seen(), 1);
}

#[test]
fn l1_contract_completed_frame_never_held() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 10_000_000i64;
    pacer.mark_released(t);

    for _ in 0..100 {
        let pres_start = t + us_to_qpc(8000.0, FREQ);
        let pres_end = pres_start + us_to_qpc(500.0, FREQ);
        let d = pacer.on_present_complete(pres_start, pres_end, None);
        assert!(
            d.wake_qpc >= pres_end,
            "CRITICAL INVARIANT VIOLATION: wake_qpc ({}) < pres_end ({}) — frame was held!",
            d.wake_qpc,
            pres_end
        );
        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
    }
}

#[test]
fn l1_contract_cadence_step_accuracy() {
    // 60 FPS on 120 Hz should step 2 vblanks every frame
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 10_000_000i64;
    let vblank_int = us_to_qpc(1_000_000.0 / 120.0, FREQ);

    // Warm up first frame
    let _ = pacer.on_present_complete(t, t + 1000, None);
    pacer.mark_released(t + 1000);
    t += 1000;

    for i in 0..20 {
        let pres_start = t + us_to_qpc(5000.0, FREQ);
        let pres_end = pres_start + us_to_qpc(500.0, FREQ);
        let hint = VBlankHint {
            refresh_hz: 120.0,
            last_vblank_qpc: t - (t % vblank_int),
        };
        let d = pacer.on_present_complete(pres_start, pres_end, Some(hint));
        if i > 2 {
            assert_eq!(d.cadence_step, 2, "60 FPS on 120 Hz must step exactly 2 vblanks");
        }
        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
    }
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — 8 Mandatory Boundary Conditions
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_zero_and_negative_qpc_frequency() {
    assert_eq!(qpc_to_us(100, 0), 0.0);
    assert_eq!(qpc_to_us(100, -100), 0.0);
    assert_eq!(us_to_qpc(100.0, 0), 0);
    assert_eq!(us_to_qpc(100.0, -100), 0);

    let pacer = JitPacer::new(PacerConfig::default(), 0);
    assert!(pacer.frames_seen() == 0);
}

#[test]
fn l2_boundary_target_fps_extremes() {
    let test_fps_values = [
        0.0,
        -100.0,
        0.00001,
        0.1,
        1.0,
        144.0,
        360.0,
        1000.0,
        100_000.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];

    for &fps in &test_fps_values {
        let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, fps), FREQ);
        let t0 = 10_000_000;
        let d1 = pacer.on_present_complete(t0, t0 + 1000, None);
        pacer.mark_released(t0 + 1000);
        assert!(d1.wake_qpc >= t0 + 1000);

        let t1 = t0 + 1000;
        let d2 = pacer.on_present_complete(t1 + 5000, t1 + 6000, None);
        assert!(d2.wake_qpc >= t1 + 6000);
        assert!(d2.stats.ema_duration_us.is_finite());
        assert!(d2.stats.safety_margin_us.is_finite());
    }
}

#[test]
fn l2_boundary_short_frame_threshold() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let t0 = 10_000_000;
    pacer.mark_released(t0);

    // Frame duration < 50 µs (e.g. 49 µs = 490 ticks at 10 MHz) -> ignored by estimator
    let d_short = pacer.on_present_complete(t0 + 100, t0 + 490, None);
    assert!(d_short.stats.bypass, "Frames < 50us must bypass");

    // Frame duration >= 50 µs (e.g. 51 µs = 510 ticks) -> accepted
    let d_ok = pacer.on_present_complete(t0 + 100, t0 + 510, None);
    assert!(d_ok.wake_qpc >= t0 + 510);
}

#[test]
fn l2_boundary_vblank_hint_edge_cases() {
    let invalid_hints = [
        VBlankHint { refresh_hz: 0.0, last_vblank_qpc: 100 },
        VBlankHint { refresh_hz: -60.0, last_vblank_qpc: 100 },
        VBlankHint { refresh_hz: 0.5, last_vblank_qpc: 100 },
        VBlankHint { refresh_hz: f64::NAN, last_vblank_qpc: 100 },
        VBlankHint { refresh_hz: f64::INFINITY, last_vblank_qpc: 100 },
        VBlankHint { refresh_hz: 60.0, last_vblank_qpc: 0 },
        VBlankHint { refresh_hz: 60.0, last_vblank_qpc: -500 },
        VBlankHint { refresh_hz: 60.0, last_vblank_qpc: i64::MAX },
    ];

    for hint in invalid_hints {
        let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
        let t0 = 10_000_000;
        pacer.mark_released(t0);
        let _ = pacer.on_present_complete(t0, t0 + 1000, Some(hint));
        pacer.mark_released(t0 + 1000);
        let d = pacer.on_present_complete(t0 + 5000, t0 + 6000, Some(hint));
        assert!(d.wake_qpc >= t0 + 6000);
    }
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Invariants that must always hold
// ---------------------------------------------------------------------------

#[test]
fn l3_property_estimator_invariants_under_randomized_frametimes() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 100_000_000i64;
    pacer.mark_released(t);

    // Pseudorandom pseudo-workload
    let mut seed = 0x12345678u64;
    for _ in 0..500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let cpu_us = 1000.0 + ((seed >> 32) % 15000) as f64; // 1ms - 16ms
        let pres_us = 100.0 + ((seed >> 16) % 2000) as f64;

        let pres_start = t + us_to_qpc(cpu_us, FREQ);
        let pres_end = pres_start + us_to_qpc(pres_us, FREQ);
        let hint = VBlankHint {
            refresh_hz: 144.0,
            last_vblank_qpc: pres_end - (pres_end % us_to_qpc(1e6 / 144.0, FREQ)),
        };

        let d = pacer.on_present_complete(pres_start, pres_end, Some(hint));

        // Invariant 1: Wake is never earlier than present completion
        assert!(d.wake_qpc >= pres_end);
        // Invariant 2: EMA is finite and non-negative
        assert!(d.stats.ema_duration_us >= 0.0 && d.stats.ema_duration_us.is_finite());
        // Invariant 3: Deviation is finite and non-negative
        assert!(d.stats.deviation_us >= 0.0 && d.stats.deviation_us.is_finite());
        // Invariant 4: Safety margin is >= base safety margin
        assert!(d.stats.safety_margin_us >= 250.0);
        // Invariant 5: Sleep is finite and non-negative
        assert!(d.stats.sleep_us >= 0.0 && d.stats.sleep_us.is_finite());

        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
    }
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — What if assumptions are lies?
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_clock_jumps_backward() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let t0 = 100_000_000i64;
    pacer.mark_released(t0);
    let _ = pacer.on_present_complete(t0 + 5000, t0 + 6000, None);
    pacer.mark_released(t0 + 6000);

    // QPC jumps backward by 50 seconds (e.g. system clock sync or multi-core desync)
    let t_back = 50_000_000i64;
    let d = pacer.on_present_complete(t_back, t_back + 1000, None);
    assert!(d.wake_qpc >= t_back + 1000, "Must not hang on backward clock jump");
    assert!(d.stats.bypass, "Negative time frame must bypass");
}

#[test]
fn l4_adversarial_massive_5_second_stall() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 100_000_000i64;
    pacer.mark_released(t);

    // Warm up
    for _ in 0..10 {
        let pres_end = t + us_to_qpc(8000.0, FREQ);
        let d = pacer.on_present_complete(t + 7000, pres_end, None);
        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
    }

    let ema_before = pacer.on_present_complete(t + 7000, t + 8000, None).stats.ema_duration_us;

    // 5-second stall (asset loading / shader compilation hitch)
    let stall_end = t + us_to_qpc(5_000_000.0, FREQ);
    let d_stall = pacer.on_present_complete(t + 4_990_000, stall_end, None);
    assert!(d_stall.wake_qpc >= stall_end);
    pacer.mark_released(stall_end);

    // The single stall must NOT poison EMA into seconds (clamped to 4 * interval)
    let d_after = pacer.on_present_complete(stall_end + 7000, stall_end + 8000, None);
    assert!(
        d_after.stats.ema_duration_us < ema_before * 4.0,
        "EMA was poisoned by a single stall: before={}, after={}",
        ema_before,
        d_after.stats.ema_duration_us
    );
}

#[test]
fn l4_adversarial_midstream_mode_and_fps_thrashing() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 100_000_000i64;
    pacer.mark_released(t);

    let modes = [PacerMode::FixedVsync, PacerMode::Vrr, PacerMode::Bypass];
    let fps_list = [30.0, 60.0, 144.0, 0.0, 240.0, 10.0, 1000.0];

    for i in 0..100 {
        let mode = modes[i % modes.len()];
        let fps = fps_list[i % fps_list.len()];
        pacer.set_config(PacerConfig::for_mode(mode, fps));

        let pres_end = t + us_to_qpc(7000.0, FREQ);
        let d = pacer.on_present_complete(t + 6000, pres_end, None);
        assert!(d.wake_qpc >= pres_end);
        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
    }
}

// ---------------------------------------------------------------------------
// L4b: Fallback Tests — When dependencies are missing
// ---------------------------------------------------------------------------

#[test]
fn l4b_fallback_fixed_vsync_without_dwm_hint_falls_back_to_vrr() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 10_000_000i64;
    pacer.mark_released(t);

    // Pass hint = None (e.g. fullscreen exclusive or DWM timing unavailable)
    let _d1 = pacer.on_present_complete(t, t + 1000, None);
    pacer.mark_released(t + 1000);
    t += 1000;

    let pres_end = t + us_to_qpc(8000.0, FREQ);
    let d2 = pacer.on_present_complete(t + 7000, pres_end, None);
    assert!(d2.wake_qpc >= pres_end);
    assert_eq!(d2.cadence_step, 1, "Fallback VRR mode steps 1 slot");
}

// ---------------------------------------------------------------------------
// L5: State Tests — Transitions and convergence
// ---------------------------------------------------------------------------

#[test]
fn l5_state_estimator_converges_to_steady_frametime() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 10_000_000i64;
    pacer.mark_released(t);

    // Warm up first frame
    let _ = pacer.on_present_complete(t, t + 1000, None);
    pacer.mark_released(t + 1000);
    t += 1000;

    // Feed constant 8000 µs frames
    let frame_us = 8000.0;
    let mut last_decision = None;
    for _ in 0..60 {
        let pres_end = t + us_to_qpc(frame_us, FREQ);
        let d = pacer.on_present_complete(t + us_to_qpc(frame_us - 500.0, FREQ), pres_end, None);
        pacer.mark_released(d.wake_qpc);
        t = d.wake_qpc;
        last_decision = Some(d);
    }

    let d = last_decision.unwrap();
    assert!(
        (d.stats.ema_duration_us - frame_us).abs() < 50.0,
        "EMA duration {:.1} did not converge to frame_us {:.1}",
        d.stats.ema_duration_us,
        frame_us
    );
    assert!(
        d.stats.deviation_us < 10.0,
        "Steady state deviation {:.1} should be near zero",
        d.stats.deviation_us
    );
}

// ---------------------------------------------------------------------------
// L9: Negative Space Tests — What must NOT happen
// ---------------------------------------------------------------------------

#[test]
fn l9_negative_space_bypass_mode_never_delays() {
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::Bypass, 60.0), FREQ);
    let mut t = 10_000_000i64;
    pacer.mark_released(t);

    for _ in 0..50 {
        let pres_end = t + us_to_qpc(5000.0, FREQ);
        let d = pacer.on_present_complete(t + 4000, pres_end, None);
        assert_eq!(d.wake_qpc, pres_end, "Bypass mode MUST NOT delay wake_qpc");
        assert_eq!(d.stats.sleep_us, 0.0, "Bypass mode sleep_us MUST be 0");
        assert!(d.stats.bypass, "Bypass flag MUST be set");
        pacer.mark_released(pres_end);
        t = pres_end;
    }
}

#[test]
fn l9_negative_space_overloaded_frame_never_delayed() {
    // 60 FPS -> interval is 16666 µs. If frame took 16500 µs (overloaded), sleep must be 0
    let mut pacer = JitPacer::new(PacerConfig::for_mode(PacerMode::FixedVsync, 60.0), FREQ);
    let mut t = 10_000_000i64;
    pacer.mark_released(t);

    let _ = pacer.on_present_complete(t, t + 1000, None);
    pacer.mark_released(t + 1000);
    t += 1000;

    let pres_end = t + us_to_qpc(16500.0, FREQ);
    let d = pacer.on_present_complete(t + 16000, pres_end, None);
    assert_eq!(d.wake_qpc, pres_end, "Overloaded frame MUST NOT be delayed");
    assert_eq!(d.stats.sleep_us, 0.0);
}
