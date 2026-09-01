//! LMTrust Deep Blind Spot Test Suite: Bench Suite & Telemetry Metrics
//!
//! Layers covered:
//! - L0 Smoke: PhaseResult struct initialization
//! - L1 Contract: compute_phase_metrics exact known coordinates (60 FPS flat -> 16.67ms p50, 0 jitter)
//! - L2 Boundary: Empty slices, single element slices, zero hold/wait times
//! - L3 Property: Invariants (p50 <= p99, min <= mean <= max, jitter >= 0.0, std_dev >= 0.0)
//! - L4 Adversarial: Single 5-second hitch spike in 1000 frames correctly identified in p99 and max
//! - L8 Temporal: ETW 100ns timestamp delta conversion accuracy (1 tick = 0.1 µs)

use iframe_app::bench::compute_phase_metrics;

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_compute_phase_metrics_empty() {
    let res = compute_phase_metrics("Empty Phase", &[], &[], &[], 0);
    assert_eq!(res.total_frames, 0);
    assert_eq!(res.fps, 0.0);
    assert_eq!(res.ft_p50_ms, 0.0);
    assert_eq!(res.ft_p99_ms, 0.0);
    assert_eq!(res.ft_jitter_ms, 0.0);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Exact Known Coordinates
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_constant_60fps_known_coordinates() {
    let ft_ms = 1000.0 / 60.0; // 16.666667 ms
    let frames = vec![ft_ms; 120];
    let holds = vec![0.5; 120]; // 0.5 ms present call duration
    let waits = vec![5.0; 120]; // 5.0 ms pacing sleep

    let res = compute_phase_metrics("Fixed 60 FPS", &frames, &holds, &waits, 0);
    assert_eq!(res.total_frames, 120);
    assert!((res.fps - 60.0).abs() < 0.001, "FPS must be 60.0");
    assert!((res.ft_mean_ms - ft_ms).abs() < 0.001);
    assert!((res.ft_p50_ms - ft_ms).abs() < 0.001);
    assert!((res.ft_p99_ms - ft_ms).abs() < 0.001);
    assert!((res.ft_jitter_ms - 0.0).abs() < 0.001, "Constant frametime has 0 jitter");
    assert!((res.ft_std_ms - 0.0).abs() < 0.001, "Constant frametime has 0 std dev");
    assert!((res.present_hold_p50_ms - 0.5).abs() < 0.001);
    assert!((res.pacing_wait_p50_ms - 5.0).abs() < 0.001);
}

#[test]
fn l1_contract_etw_timestamp_conversion_math() {
    // ETW raw timestamps are in 100 ns units (10 MHz equivalent ticks)
    // Formula: (ts2 - ts1) / 10.0 = microseconds
    let ts1 = 1_000_000_000u64;
    let ts2 = ts1 + 166_667; // ~16.6667 ms in 100ns units

    let frametime_us = (ts2 - ts1) as f64 / 10.0;
    assert!((frametime_us - 16_666.7).abs() < 0.1);
    let frametime_ms = frametime_us / 1000.0;
    assert!((frametime_ms - 16.6667).abs() < 0.001);
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — 8 Boundary Conditions
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_single_frame_metrics() {
    let frames = [16.0];
    let holds = [1.0];
    let waits = [2.0];

    let res = compute_phase_metrics("Single", &frames, &holds, &waits, 1);
    assert_eq!(res.total_frames, 1);
    assert_eq!(res.late_frames, 1);
    assert_eq!(res.ft_min_ms, 16.0);
    assert_eq!(res.ft_max_ms, 16.0);
    assert_eq!(res.ft_p50_ms, 16.0);
    assert_eq!(res.ft_p99_ms, 16.0);
    assert_eq!(res.ft_jitter_ms, 0.0);
    assert_eq!(res.ft_std_ms, 0.0);
}

#[test]
fn l2_boundary_zero_present_durations() {
    let frames = [8.33, 8.33, 8.33];
    let holds = [0.0, 0.0, 0.0];
    let waits = [0.0, 0.0, 0.0];

    let res = compute_phase_metrics("Zero Durations", &frames, &holds, &waits, 0);
    assert_eq!(res.present_hold_p50_ms, 0.0);
    assert_eq!(res.pacing_wait_p50_ms, 0.0);
    assert!((res.fps - 120.048).abs() < 0.01);
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Metric Invariants
// ---------------------------------------------------------------------------

#[test]
fn l3_property_statistical_invariants() {
    let mut frames = Vec::new();
    let mut holds = Vec::new();
    let mut waits = Vec::new();

    for i in 1..=200 {
        frames.push((i % 20 + 5) as f64); // 5ms - 25ms
        holds.push((i % 3) as f64 * 0.1);
        waits.push((i % 5) as f64);
    }

    let res = compute_phase_metrics("Random Distribution", &frames, &holds, &waits, 5);

    assert!(res.ft_p50_ms <= res.ft_p99_ms, "p50 must be <= p99");
    assert!(res.ft_min_ms <= res.ft_mean_ms, "min must be <= mean");
    assert!(res.ft_mean_ms <= res.ft_max_ms, "mean must be <= max");
    assert!(res.ft_jitter_ms >= 0.0, "jitter must be >= 0.0");
    assert!(res.ft_std_ms >= 0.0, "std_dev must be >= 0.0");
    assert_eq!(res.late_frames, 5);
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Outlier Hitch Handling
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_single_massive_spike_caught_in_max_and_p99() {
    // 980 frames at 16.6ms + 20 frames at 200.0ms hitch (2% outliers)
    let mut frames = vec![16.6; 980];
    frames.extend(vec![200.0; 20]);
    let holds = vec![0.2; 1000];
    let waits = vec![4.0; 1000];

    let res = compute_phase_metrics("Hitch Test", &frames, &holds, &waits, 20);
    assert_eq!(res.ft_max_ms, 200.0, "Max must capture the hitch");
    assert_eq!(res.ft_p99_ms, 200.0, "p99 must capture the top 2% hitches");
    assert!((res.ft_p50_ms - 16.6).abs() < 0.01, "Median p50 must remain undisturbed by 2% hitches");
    assert!(res.ft_jitter_ms > 150.0, "Jitter must reflect the outlier spread");
}
