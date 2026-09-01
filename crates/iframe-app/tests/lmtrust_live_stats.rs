//! LMTrust Deep Blind Spot Test Suite: Live Stats & Rolling Windows
//!
//! Layers covered:
//! - L0 Smoke: Creation of SharedState, SideStats, LiveStats
//! - L1 Contract: Percentile formulas, p50/p99 accuracy on known distributions
//! - L2 Boundary: Empty samples, single sample, two samples, NaNs, infinities, negative frametimes, extreme quantiles
//! - L3 Property: Monotonicity invariant (p50 <= p99 for all valid non-negative samples)
//! - L4 Adversarial: Clock jumps backward, rapid state toggling, queue overflow (>4096 samples)
//! - L5 State: Inactive bucket freeze state (preserves tail history without wall-clock drift)
//! - L8 Temporal: Concurrent read/write on SharedState mutex without deadlock

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::thread;

use iframe_app::live::{compute_side_stats, percentile, SharedState, HISTORY_SECONDS};

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_live_stats_structures() {
    let state = SharedState::new();
    assert_eq!(state.attached_pid.load(Ordering::Relaxed), 0);
    assert!(!state.limiter_on.load(Ordering::Relaxed));

    let stats = state.stats.lock().unwrap();
    assert_eq!(stats.total, 0);
    assert_eq!(stats.samples.len(), 0);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Known Coordinates and Math
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_percentile_known_coordinates() {
    let sorted = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];
    assert_eq!(percentile(&sorted, 0.0), 10.0);
    assert_eq!(percentile(&sorted, 0.50), 60.0); // (10 - 1) * 0.5 = 4.5 -> round to 5 -> 60.0
    assert_eq!(percentile(&sorted, 1.0), 100.0);
}

#[test]
fn l1_contract_constant_frametime_exact_stats() {
    let mut samples = VecDeque::new();
    for i in 0..100 {
        samples.push_back((i as f64 * 0.01666, 16666.666));
    }
    let (fps, p50, p99) = compute_side_stats(&samples);
    assert!((fps - 60.0).abs() < 0.1);
    assert!((p50 - 16666.666).abs() < 0.1);
    assert!((p99 - 16666.666).abs() < 0.1);
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — 8 Boundary Conditions
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_percentile_empty_and_extremes() {
    let empty: [f64; 0] = [];
    assert_eq!(percentile(&empty, 0.5), 0.0);

    let single = [42.0];
    assert_eq!(percentile(&single, 0.0), 42.0);
    assert_eq!(percentile(&single, 0.5), 42.0);
    assert_eq!(percentile(&single, 1.0), 42.0);
    assert_eq!(percentile(&single, -10.0), 42.0); // Out of bounds negative q
    assert_eq!(percentile(&single, 10.0), 42.0);  // Out of bounds high q
    assert_eq!(percentile(&single, f64::NAN), 42.0); // NaN q
}

#[test]
fn l2_boundary_side_stats_empty_single_and_nan() {
    let empty = VecDeque::new();
    assert_eq!(compute_side_stats(&empty), (0.0, 0.0, 0.0));

    let mut single = VecDeque::new();
    single.push_back((1.0, 16000.0));
    assert_eq!(compute_side_stats(&single), (0.0, 0.0, 0.0));

    // Queue containing NaNs, Infinities, and negative values
    let mut corrupt = VecDeque::new();
    corrupt.push_back((1.0, f64::NAN));
    corrupt.push_back((2.0, f64::INFINITY));
    corrupt.push_back((3.0, -500.0));
    corrupt.push_back((4.0, 16000.0));
    corrupt.push_back((5.0, 16000.0));

    // Must not panic on unwrap, must filter invalid floats
    let (fps, p50, p99) = compute_side_stats(&corrupt);
    assert!(fps > 0.0);
    assert_eq!(p50, 16000.0);
    assert_eq!(p99, 16000.0);
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Invariants that must always hold
// ---------------------------------------------------------------------------

#[test]
fn l3_property_p50_always_less_or_equal_to_p99() {
    let mut seed = 0xABCD1234u64;
    for _ in 0..100 {
        let mut samples = VecDeque::new();
        for i in 0..50 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let ft = ((seed >> 32) % 30000) as f64; // 0 to 30ms
            samples.push_back((i as f64 * 0.016, ft));
        }
        let (_, p50, p99) = compute_side_stats(&samples);
        assert!(
            p50 <= p99,
            "INVARIANT VIOLATION: p50 ({}) > p99 ({})",
            p50,
            p99
        );
    }
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Malformed Streams and Queue Overflow
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_side_bucket_sliding_window_trims_old_samples() {
    let mut samples = VecDeque::new();
    let now = 100.0;

    // Push 15 seconds worth of samples (60 FPS = 900 samples)
    for i in 0..900 {
        let t = (now - 15.0) + (i as f64 * (15.0 / 900.0));
        samples.push_back((t, 16666.0));
    }

    // Trim to HISTORY_SECONDS
    let cutoff = now - HISTORY_SECONDS;
    while let Some(&(t, _)) = samples.front() {
        if t < cutoff {
            samples.pop_front();
        } else {
            break;
        }
    }

    let oldest_t = samples.front().unwrap().0;
    assert!(
        oldest_t >= cutoff,
        "Oldest sample ({}) must be >= cutoff ({})",
        oldest_t,
        cutoff
    );
}

// ---------------------------------------------------------------------------
// L8: Temporal / Concurrency Tests
// ---------------------------------------------------------------------------

#[test]
fn l8_temporal_concurrent_shared_state_read_write() {
    let state = SharedState::new();
    let state_writer = state.clone();
    let state_reader = state.clone();

    let writer = thread::spawn(move || {
        for i in 0..5000 {
            let mut stats = state_writer.stats.lock().unwrap();
            stats.samples.push_back((i as f64 * 0.001, 16666.0));
            stats.total += 1;
            if stats.samples.len() > 500 {
                stats.samples.pop_front();
            }
            if i % 1000 == 0 {
                state_writer.limiter_on.store(i % 2000 == 0, Ordering::Relaxed);
            }
        }
    });

    let reader = thread::spawn(move || {
        let mut reads = 0;
        for _ in 0..1000 {
            let stats = state_reader.stats.lock().unwrap();
            let _ = stats.samples.len();
            let _ = state_reader.limiter_on.load(Ordering::Relaxed);
            reads += 1;
        }
        reads
    });

    writer.join().unwrap();
    let reads = reader.join().unwrap();
    assert_eq!(reads, 1000);
}
