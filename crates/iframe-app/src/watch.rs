//! Telemetry watcher: attach to the shared ring and print live stats.

use std::time::{Duration, Instant};
use windows::Win32::System::Performance::QueryPerformanceFrequency;

use crate::live::percentile;
use crate::sm_host;
use iframe_common::shared_mem::TelemetryFrame;

fn qpc_frequency() -> i64 {
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 { 10_000_000 } else { f }
}

/// Drain the ring for `seconds` and print per-second stats.
pub fn watch(pid: u32, seconds: u64) -> Result<(), String> {
    let mapping = sm_host::open(pid)?;
    let ring = &mapping.ring;
    println!("watching pid {pid} for {seconds}s");

    let mut scratch: Vec<TelemetryFrame> = vec![TelemetryFrame::default(); 4096];
    let mut present_starts: Vec<i64> = Vec::with_capacity(4096);
    let freq = qpc_frequency();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_report = Instant::now();

    while Instant::now() < deadline {
        let n = sm_host::drain(ring, &mut scratch);
        for frame in &scratch[..n] {
            present_starts.push(frame.present_start_qpc);
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            print_second(&present_starts, freq);
            present_starts.clear();
            last_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !present_starts.is_empty() {
        print_second(&present_starts, freq);
    }
    Ok(())
}

fn print_second(present_starts: &[i64], freq: i64) {
    if present_starts.len() < 2 {
        println!("  (no presents yet)");
        return;
    }
    let mut diffs: Vec<f64> = present_starts
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 * 1e6 / freq as f64)
        .collect();
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = percentile(&diffs, 0.50);
    let p99 = percentile(&diffs, 0.99);
    let max = diffs.last().copied().unwrap_or(0.0);
    let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let fps = if mean > 0.0 { 1e6 / mean } else { 0.0 };
    println!(
        "presents: {:>5}  fps: {:>8.1}  frametime p50/p99/max: {:>8.2} / {:>8.2} / {:>8.2} µs",
        present_starts.len(),
        fps,
        p50,
        p99,
        max
    );
}