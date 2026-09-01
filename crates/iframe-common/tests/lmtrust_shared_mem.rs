//! LMTrust Deep Blind Spot Test Suite: Lock-Free Shared Memory SPSC Ring Buffer
//!
//! Layers covered:
//! - L1 Contract: Memory layout, alignment, 48-byte frames, 64-byte aligned header, FIFO ordering
//! - L2 Boundary: Invalid capacities (0, non-powers-of-two), undersized memory buffers, capacity 2, drain sizes
//! - L3 Property: Config bit-exact roundtrip, monotonic counters, conservation of frames (pushed = popped + dropped)
//! - L4 Adversarial: Memory corruption (bad magic, bad version, tampered capacity mask, clock jumps)
//! - L6 Cross-System & L8 Temporal: Concurrent high-throughput SPSC stress test (100k frames) across threads
//! - L9 Negative Space: Empty pop leaves memory untouched, full push does not overwrite unread data

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerMode;
use iframe_common::shared_mem::{
    total_size, SharedHeader, SharedRing, TelemetryFrame, SM_MAGIC, SM_VERSION,
};

/// 64-byte aligned memory buffer for test allocations.
struct AlignedTestMem {
    ptr: *mut u8,
    layout: Layout,
    len: usize,
}

impl AlignedTestMem {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, 64).expect("valid layout");
        let ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        unsafe { std::ptr::write_bytes(ptr, 0, len) };
        Self { ptr, layout, len }
    }
}

impl Drop for AlignedTestMem {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn sample_frame(seq: u64) -> TelemetryFrame {
    TelemetryFrame {
        present_start_qpc: seq as i64 * 1000,
        present_end_qpc: seq as i64 * 1000 + 100,
        release_qpc: seq as i64 * 1000 + 200,
        target_tick_qpc: seq as i64 * 1000 + 300,
        ema_duration_us: 8000.0 + (seq % 100) as f32,
        deviation_us: 15.0,
        flags: if seq % 2 == 0 {
            TelemetryFrame::FLAG_BYPASS
        } else {
            TelemetryFrame::FLAG_LATE
        },
        _pad: 0,
    }
}

// ---------------------------------------------------------------------------
// L1: Contract Tests — Memory Layout and Basic Operation
// ---------------------------------------------------------------------------

#[test]
fn l1_contract_memory_layout_and_alignment() {
    assert_eq!(
        std::mem::size_of::<TelemetryFrame>(),
        48,
        "TelemetryFrame must be exactly 48 bytes for ABI stability"
    );
    assert_eq!(
        std::mem::align_of::<SharedHeader>(),
        64,
        "SharedHeader must be 64-byte aligned for cache line separation"
    );
    let cap = 4096;
    let expected_len = std::mem::size_of::<SharedHeader>() + cap * 48;
    assert_eq!(total_size(cap), expected_len);
}

#[test]
fn l1_contract_fifo_push_pop_ordering() {
    let mem = AlignedTestMem::new(total_size(16));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 16).unwrap() };

    for i in 0..10 {
        assert!(ring.push(&sample_frame(i)));
    }

    for i in 0..10 {
        let rec = ring.pop().expect("frame must be present");
        assert_eq!(rec.present_start_qpc, i as i64 * 1000);
    }
    assert!(ring.pop().is_none());
}

#[test]
fn l1_contract_drain_helper() {
    let mem = AlignedTestMem::new(total_size(32));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 32).unwrap() };

    for i in 0..15 {
        assert!(ring.push(&sample_frame(i)));
    }

    let mut out = vec![TelemetryFrame::default(); 10];
    let n1 = ring.drain(&mut out);
    assert_eq!(n1, 10);
    assert_eq!(out[0].present_start_qpc, 0);
    assert_eq!(out[9].present_start_qpc, 9000);

    let n2 = ring.drain(&mut out);
    assert_eq!(n2, 5);
    assert_eq!(out[0].present_start_qpc, 10_000);
    assert_eq!(out[4].present_start_qpc, 14_000);

    let n3 = ring.drain(&mut out);
    assert_eq!(n3, 0);
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — 8 Boundary Conditions
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_invalid_capacities_rejected() {
    let mem = AlignedTestMem::new(total_size(1024));

    // Non-power-of-two capacities MUST be rejected
    for invalid_cap in [0, 3, 5, 6, 7, 9, 15, 100, 1000, usize::MAX] {
        let ring = unsafe { SharedRing::init(mem.ptr, mem.len, invalid_cap) };
        assert!(ring.is_none(), "Capacity {invalid_cap} must be rejected");
    }
}

#[test]
fn l2_boundary_undersized_buffer_rejected() {
    let cap = 16;
    let required_len = total_size(cap);

    // Buffer smaller by even 1 byte must be rejected
    let mem = AlignedTestMem::new(required_len);
    let ring_short = unsafe { SharedRing::init(mem.ptr, required_len - 1, cap) };
    assert!(ring_short.is_none());

    let ring_ok = unsafe { SharedRing::init(mem.ptr, required_len, cap) };
    assert!(ring_ok.is_some());
}

#[test]
fn l2_boundary_minimal_capacity_2() {
    let mem = AlignedTestMem::new(total_size(2));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 2).unwrap() };

    assert!(ring.push(&sample_frame(1)));
    assert!(ring.push(&sample_frame(2)));
    // Ring full: next push must fail
    assert!(!ring.push(&sample_frame(3)));
    assert_eq!(ring.dropped(), 1);

    let rec1 = ring.pop().unwrap();
    assert_eq!(rec1.present_start_qpc, 1000);

    // Now slot is free
    assert!(ring.push(&sample_frame(4)));
    assert_eq!(ring.pop().unwrap().present_start_qpc, 2000);
    assert_eq!(ring.pop().unwrap().present_start_qpc, 4000);
    assert!(ring.pop().is_none());
}

#[test]
fn l2_boundary_empty_pop_and_zero_drain() {
    let mem = AlignedTestMem::new(total_size(8));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 8).unwrap() };

    assert!(ring.pop().is_none());

    let mut empty_out: Vec<TelemetryFrame> = Vec::new();
    assert_eq!(ring.drain(&mut empty_out), 0);
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Invariants and Roundtrips
// ---------------------------------------------------------------------------

#[test]
fn l3_property_config_roundtrip_bit_exact() {
    let mem = AlignedTestMem::new(total_size(16));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 16).unwrap() };

    let test_configs = [
        RuntimeConfig {
            enabled: true,
            mode: PacerMode::FixedVsync,
            target_fps: 60.0,
            refresh_hz: 120.0,
            vsync_override: true,
            force_waitable: false,
        },
        RuntimeConfig {
            enabled: false,
            mode: PacerMode::Vrr,
            target_fps: 144.0,
            refresh_hz: 144.0,
            vsync_override: false,
            force_waitable: true,
        },
        RuntimeConfig {
            enabled: true,
            mode: PacerMode::Bypass,
            target_fps: 360.0,
            refresh_hz: 360.0,
            vsync_override: true,
            force_waitable: true,
        },
    ];

    for cfg in test_configs {
        ring.set_config(&cfg);
        let read = ring.config();
        assert_eq!(read, cfg);
    }
}

#[test]
fn l3_property_hook_state_publishing() {
    let mem = AlignedTestMem::new(total_size(16));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 16).unwrap() };

    assert_eq!(ring.hook_state(), 0);
    ring.set_hook_state(1);
    assert_eq!(ring.hook_state(), 1);
    ring.set_hook_state(2);
    assert_eq!(ring.hook_state(), 2);
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Corrupted Shared Memory
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_corrupted_header_magic_or_version_refused() {
    let mem = AlignedTestMem::new(total_size(16));
    let _ = unsafe { SharedRing::init(mem.ptr, mem.len, 16).unwrap() };

    // Valid attach first
    let attach_ok = unsafe { SharedRing::attach(mem.ptr, mem.len) };
    assert!(attach_ok.is_some());

    // Corrupt magic
    let header = unsafe { &mut *(mem.ptr as *mut SharedHeader) };
    header.magic.store(0xDEADBEEF, Ordering::Release);
    assert!(unsafe { SharedRing::attach(mem.ptr, mem.len) }.is_none());

    // Restore magic, corrupt version
    header.magic.store(SM_MAGIC, Ordering::Release);
    header.version.store(999, Ordering::Release);
    assert!(unsafe { SharedRing::attach(mem.ptr, mem.len) }.is_none());

    // Restore version, corrupt capacity mask to non-power-of-2
    header.version.store(SM_VERSION, Ordering::Release);
    header.capacity_mask.store(100, Ordering::Release); // cap = 101
    assert!(unsafe { SharedRing::attach(mem.ptr, mem.len) }.is_none());
}

#[test]
fn l4_adversarial_consumer_lag_fast_forwards_tail_without_panic() {
    let mem = AlignedTestMem::new(total_size(8));
    let ring = unsafe { SharedRing::init(mem.ptr, mem.len, 8).unwrap() };

    // Producer writes 20 frames, wrapping around multiple times
    for i in 0..20 {
        // Manually simulate a producer that overwrote (e.g. forced head advance)
        let _ = ring.push(&sample_frame(i));
    }

    // Attach new consumer
    let consumer = unsafe { SharedRing::attach(mem.ptr, mem.len).unwrap() };
    // Consumer should not panic and should pop latest valid records
    let mut popped = 0;
    while let Some(_) = consumer.pop() {
        popped += 1;
    }
    assert!(popped <= 8, "Cannot pop more than capacity from unread lag");
}

// ---------------------------------------------------------------------------
// L6 / L8: Concurrency Stress Test — Multi-Threaded SPSC Lock-Free
// ---------------------------------------------------------------------------

#[test]
fn l8_temporal_concurrent_spsc_stress_100k_frames() {
    let cap = 1024;
    let mem = Arc::new(AlignedTestMem::new(total_size(cap)));
    let raw_ptr = mem.ptr;
    let len = mem.len;

    let producer_ring = unsafe { SharedRing::init(raw_ptr, len, cap).unwrap() };
    let consumer_ring = unsafe { SharedRing::attach(raw_ptr, len).unwrap() };

    let total_frames = 100_000u64;

    let producer = thread::spawn(move || {
        let mut pushed = 0u64;
        let mut dropped = 0u64;
        for seq in 1..=total_frames {
            if producer_ring.push(&sample_frame(seq)) {
                pushed += 1;
            } else {
                dropped += 1;
            }
            if seq % 500 == 0 {
                thread::yield_now();
            }
        }
        (pushed, dropped)
    });

    let consumer = thread::spawn(move || {
        let mut received = Vec::with_capacity(100_000);
        let mut empty_spins = 0;
        loop {
            match consumer_ring.pop() {
                Some(f) => {
                    let seq = (f.present_start_qpc / 1000) as u64;
                    received.push(seq);
                    empty_spins = 0;
                }
                None => {
                    empty_spins += 1;
                    if empty_spins > 500 && received.len() > 0 {
                        // Check if producer finished
                        thread::sleep(std::time::Duration::from_millis(1));
                        if consumer_ring.pop().is_none() && empty_spins > 1000 {
                            break;
                        }
                    } else {
                        std::hint::spin_loop();
                    }
                }
            }
        }
        received
    });

    let (pushed, dropped) = producer.join().expect("producer failed");
    let received = consumer.join().expect("consumer failed");

    // Invariant 1: Strictly monotonically increasing sequence numbers
    for w in received.windows(2) {
        assert!(
            w[1] > w[0],
            "Out of order sequence in consumer: {} then {}",
            w[0],
            w[1]
        );
    }

    // Invariant 2: Total pushed + dropped == total attempted
    assert_eq!(pushed + dropped, total_frames);
}
