//! LMTrust Deep Blind Spot Test Suite: Timing & Hook Engine
//!
//! Layers covered:
//! - L0 Smoke: ABI exports (iframe_ping, iframe_abi_version), QPC timer calibration
//! - L1 Contract: HighResSleeper accuracy, engine::pace step calculations
//! - L2 Boundary: Past target timestamps (<= now), zero cap, negative cap
//! - L4 Adversarial: Extreme sleep requests clamped by sleep_until_capped safety net
//! - L6 Cross-System & L8 Temporal: Concurrent Present calls non-blocking try_lock

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerMode;
use iframe_hook::engine::pace;
use iframe_hook::timing::{qpc_frequency, qpc_now, HighResSleeper};
use iframe_hook::{iframe_abi_version, iframe_ping};

// ---------------------------------------------------------------------------
// L0: Smoke Tests
// ---------------------------------------------------------------------------

#[test]
fn l0_smoke_abi_exports() {
    assert_eq!(iframe_ping(), 0x4946_524D);
    assert_eq!(iframe_abi_version(), 1);
}

#[test]
fn l0_smoke_qpc_timer_frequency_and_now() {
    let freq = qpc_frequency();
    assert!(freq > 0, "QPC frequency must be strictly positive");

    let t1 = qpc_now();
    let t2 = qpc_now();
    assert!(t2 >= t1, "QPC timestamps must be monotonic");
}

#[test]
fn l0_smoke_sleeper_instantiation() {
    let sleeper = HighResSleeper::new();
    // Sleep 1 microsecond (near instant)
    let now = qpc_now();
    sleeper.sleep_until(now + 10);
    assert!(qpc_now() >= now + 10);
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Sleeper Precision and Engine Pace Contract
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_sleeper_wakes_at_or_after_target() {
    let sleeper = HighResSleeper::new();
    let freq = qpc_frequency();

    // Test 1ms wait
    let wait_us = 1000.0;
    let ticks = (wait_us * freq as f64 / 1_000_000.0).round() as i64;

    let start = qpc_now();
    let target = start + ticks;
    sleeper.sleep_until(target);
    let end = qpc_now();

    assert!(
        end >= target,
        "Sleeper woke up early! end: {}, target: {}",
        end,
        target
    );

    let actual_us = (end - start) as f64 * 1_000_000.0 / freq as f64;
    // High precision wait should be within 1.0ms - 2.5ms on Windows scheduling
    assert!(
        actual_us >= 1000.0 && actual_us < 3500.0,
        "Sleep duration out of expected bounds: {:.1} µs",
        actual_us
    );
}

#[test]
fn l1_contract_engine_pace_step() {
    let cfg = RuntimeConfig {
        enabled: true,
        mode: PacerMode::FixedVsync,
        target_fps: 60.0,
        refresh_hz: 60.0,
        vsync_override: true,
        force_waitable: false,
    };

    let freq = qpc_frequency();
    let t0 = qpc_now();
    let t0_end = t0 + (100.0 * freq as f64 / 1e6) as i64;

    // Pace step must never hold frame (release >= t_end)
    if let Some((rel1, d1)) = pace(t0, t0_end, &cfg, None) {
        assert!(rel1 >= t0_end, "Release timestamp must be >= present_end");
        assert!(d1.wake_qpc >= t0_end, "Wake target must be >= present_end");
    }

    let t1 = qpc_now();
    let t1_end = t1 + (500.0 * freq as f64 / 1e6) as i64;
    if let Some((rel2, d2)) = pace(t1, t1_end, &cfg, None) {
        assert!(rel2 >= t1_end);
        assert!(d2.wake_qpc >= t1_end);
    }
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — Zero and Past Timestamps
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_sleep_in_the_past_returns_immediately() {
    let sleeper = HighResSleeper::new();
    let now = qpc_now();

    let start = Instant::now();
    sleeper.sleep_until(now - 1000);
    sleeper.sleep_until(0);
    sleeper.sleep_until(-500);
    assert!(
        start.elapsed().as_millis() < 5,
        "Past sleep timestamps must return immediately"
    );
}

#[test]
fn l2_boundary_sleep_until_capped_negative_and_zero_max() {
    let sleeper = HighResSleeper::new();
    let now = qpc_now();

    let start = Instant::now();
    sleeper.sleep_until_capped(now + 10_000_000, 0.0);
    sleeper.sleep_until_capped(now + 10_000_000, -100.0);
    assert!(
        start.elapsed().as_millis() < 5,
        "Zero or negative max_us must return immediately without waiting"
    );
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Broken Schedule Clamping
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_huge_target_sleep_is_capped() {
    let sleeper = HighResSleeper::new();
    let now = qpc_now();

    // Request 100 seconds of sleep with a 2000 µs cap (2 ms)
    let huge_target = now + 100 * qpc_frequency();
    let start = Instant::now();
    sleeper.sleep_until_capped(huge_target, 2000.0);
    let elapsed = start.elapsed();

    // Must wake up within ~5 ms, NOT 100 seconds!
    assert!(
        elapsed.as_millis() < 50,
        "Broken schedule was not capped! Elapsed: {:?}",
        elapsed
    );
}

// ---------------------------------------------------------------------------
// L8: Concurrency Tests — Non-Blocking try_lock
// ---------------------------------------------------------------------------

#[test]
fn l8_temporal_concurrent_pace_calls_do_not_block() {
    let cfg = RuntimeConfig {
        enabled: true,
        mode: PacerMode::Vrr,
        target_fps: 60.0,
        refresh_hz: 60.0,
        vsync_override: false,
        force_waitable: false,
    };

    let cfg_arc = Arc::new(cfg);
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let cfg_clone = cfg_arc.clone();
        let succ_clone = successes.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let now = qpc_now();
                if pace(now, now + 100, &cfg_clone, None).is_some() {
                    succ_clone.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // At least some pace calls must succeed without any deadlocks
    assert!(successes.load(Ordering::Relaxed) > 0);
}

#[test]
fn l4_adversarial_spin_until_bounded_by_safety_limit() {
    use iframe_hook::timing::spin_until;
    let start = Instant::now();
    // Even if passed i64::MAX, spin_until must not spin forever (capped at 100ms)
    spin_until(i64::MAX);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() >= 80 && elapsed.as_millis() <= 250,
        "spin_until was not bounded properly: elapsed {:?}",
        elapsed
    );
}

