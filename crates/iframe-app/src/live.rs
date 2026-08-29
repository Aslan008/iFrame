//! Live telemetry: a background drain thread per attached game + shared
//! stats the UI reads every frame.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::System::Performance::QueryPerformanceCounter;

use crate::sm_host;
use iframe_common::shared_mem::TelemetryFrame;

/// How much frametime history the graph keeps.
pub const HISTORY_SECONDS: f64 = 10.0;

#[derive(Default)]
pub struct LiveStats {
    /// `(present_start in QPC seconds, frametime in µs)`, oldest first.
    pub samples: VecDeque<(f64, f64)>,
    pub fps: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    pub late: u64,
    pub bypass: u64,
    pub total: u64,
}

pub struct SharedState {
    pub stats: Mutex<LiveStats>,
    /// PID of the attached game, 0 = none.
    pub attached_pid: AtomicU32,
    /// Mirrors the shared config so the tray/hotkey can toggle the limiter.
    pub limiter_on: AtomicBool,
}

impl SharedState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stats: Mutex::new(LiveStats::default()),
            attached_pid: AtomicU32::new(0),
            limiter_on: AtomicBool::new(false),
        })
    }
}

fn qpc_freq() -> i64 {
    let mut f = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 { 10_000_000 } else { f }
}

/// Current QPC time in seconds — the x-axis unit of the live graph.
pub fn qpc_seconds() -> f64 {
    qpc_now() as f64 / qpc_freq() as f64
}

fn qpc_now() -> i64 {
    let mut v = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}

/// Spawn the drain thread for `pid`. Exits when `attached_pid` changes away
/// from `pid` (detach / switch games).
pub fn spawn_worker(pid: u32, state: Arc<SharedState>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("iframe-telemetry-{pid}"))
        .spawn(move || worker(pid, state))
        .expect("spawn telemetry worker")
}

fn worker(pid: u32, state: Arc<SharedState>) {
    let Ok(mapping) = sm_host::open(pid) else {
        return;
    };
    let ring = &mapping.ring;
    let freq = qpc_freq() as f64;
    let mut scratch = vec![TelemetryFrame::default(); 512];
    let mut last_present: Option<i64> = None;
    let mut late = 0u64;
    let mut bypass = 0u64;
    let mut total = 0u64;
    let mut samples: VecDeque<(f64, f64)> = VecDeque::with_capacity(2048);

    loop {
        if state.attached_pid.load(Ordering::Relaxed) != pid {
            break;
        }
        let n = sm_host::drain(ring, &mut scratch);
        for f in &scratch[..n] {
            if let Some(prev) = last_present {
                let ft_us = (f.present_start_qpc - prev) as f64 * 1e6 / freq;
                if ft_us > 0.0 && ft_us < 1_000_000.0 {
                    samples.push_back((f.present_start_qpc as f64 / freq, ft_us));
                }
            }
            last_present = Some(f.present_start_qpc);
            total += 1;
            if f.flags & TelemetryFrame::FLAG_LATE != 0 {
                late += 1;
            }
            if f.flags & TelemetryFrame::FLAG_BYPASS != 0 {
                bypass += 1;
            }
        }

        // Trim history to the graph window.
        if let Some(&(newest, _)) = samples.back() {
            let cutoff = newest - HISTORY_SECONDS;
            while samples.front().is_some_and(|(t, _)| *t < cutoff) {
                samples.pop_front();
            }
        }

        // Stats over the last second of samples.
        let now_s = qpc_now() as f64 / freq;
        let recent: Vec<f64> = samples
            .iter()
            .filter(|(t, _)| now_s - *t <= 1.0)
            .map(|(_, ft)| *ft)
            .collect();
        let (fps, p50, p99) = if recent.len() >= 2 {
            let mean = recent.iter().sum::<f64>() / recent.len() as f64;
            let mut sorted = recent.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let p50 = sorted[sorted.len() / 2];
            let p99 = sorted[(sorted.len() as f64 * 0.99) as usize % sorted.len()];
            (1e6 / mean, p50, p99)
        } else {
            (0.0, 0.0, 0.0)
        };

        if let Ok(mut stats) = state.stats.try_lock() {
            stats.samples = samples.clone();
            stats.fps = fps;
            stats.p50_us = p50;
            stats.p99_us = p99;
            stats.late = late;
            stats.bypass = bypass;
            stats.total = total;
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}