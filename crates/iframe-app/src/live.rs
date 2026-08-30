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

/// Frametime stats of a single limiter state (A/B compare: OFF = "before",
/// ON = "after"). Percentiles span the whole bucket window (up to 10 s of
/// that state), not the rolling 1 s used for the headline stats.
#[derive(Default, Clone)]
pub struct SideStats {
    /// `(t in QPC seconds, frametime in µs)`, oldest first.
    pub samples: VecDeque<(f64, f64)>,
    pub fps: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    /// Frames seen in this state since the worker started (not windowed).
    pub total: u64,
    /// p50 of the real Present call duration (`present_end − present_start`).
    /// Unchanged by the limiter — proof the completed frame is never held.
    pub hold_p50_us: f64,
    /// p50 of the hook's pacing wait (`release − present_end`) — the delay
    /// applied to the NEXT frame's start only.
    pub wait_p50_us: f64,
    /// False for ETW telemetry (no present-duration data there).
    pub has_timing: bool,
}

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
    /// Frametime stats while the limiter was OFF ("before").
    pub off: SideStats,
    /// Frametime stats while the limiter was ON ("after").
    pub on: SideStats,
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

/// Percentile `q` (0..1) of an ascending-sorted slice (same method as watch.rs).
pub(crate) fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// FPS / p50 / p99 (µs) over one side-bucket window. Fewer than 2 samples → zeros.
pub fn compute_side_stats(samples: &VecDeque<(f64, f64)>) -> (f64, f64, f64) {
    if samples.len() < 2 {
        return (0.0, 0.0, 0.0);
    }
    let mean = samples.iter().map(|(_, ft)| *ft).sum::<f64>() / samples.len() as f64;
    let mut sorted: Vec<f64> = samples.iter().map(|(_, ft)| *ft).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = percentile(&sorted, 0.50);
    let p99 = percentile(&sorted, 0.99);
    (if mean > 0.0 { 1e6 / mean } else { 0.0 }, p50, p99)
}

/// One A/B bucket: frametime samples of a single limiter state, trimmed to
/// `HISTORY_SECONDS` relative to its own newest sample — an inactive bucket
/// freezes on the tail of its period instead of draining away with wall time.
struct SideBucket {
    samples: VecDeque<(f64, f64)>,
    holds: VecDeque<f64>,
    waits: VecDeque<f64>,
    total: u64,
}

/// Median of an unsorted queue.
fn p50_of(v: &VecDeque<f64>) -> f64 {
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    percentile(&s, 0.50)
}

const MAX_QUEUE_SAMPLES: usize = 4096;

impl SideBucket {
    fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(1024),
            holds: VecDeque::with_capacity(1024),
            waits: VecDeque::with_capacity(1024),
            total: 0,
        }
    }

    fn push(&mut self, t: f64, ft_us: f64, hold_us: f64, wait_us: f64) {
        // Reset if time went backwards (e.g. clock jump or ring wrap around)
        if let Some(&(last_t, _)) = self.samples.back() {
            if t < last_t {
                self.samples.clear();
                self.holds.clear();
                self.waits.clear();
            }
        }

        self.samples.push_back((t, ft_us));
        self.holds.push_back(hold_us);
        self.waits.push_back(wait_us);
        self.total += 1;

        let cutoff = t - HISTORY_SECONDS;
        while let Some(&(front_t, _)) = self.samples.front() {
            if front_t < cutoff || self.samples.len() > MAX_QUEUE_SAMPLES {
                self.samples.pop_front();
                self.holds.pop_front();
                self.waits.pop_front();
            } else {
                break;
            }
        }
        while self.holds.len() > self.samples.len() {
            self.holds.pop_front();
            self.waits.pop_front();
        }
    }

    fn publish(&self) -> SideStats {
        let (fps, p50, p99) = compute_side_stats(&self.samples);
        SideStats {
            samples: self.samples.clone(),
            fps,
            p50_us: p50,
            p99_us: p99,
            total: self.total,
            hold_p50_us: p50_of(&self.holds),
            wait_p50_us: p50_of(&self.waits),
            has_timing: true,
        }
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
    let mut samples: VecDeque<(f64, f64)> = VecDeque::with_capacity(1024);
    let mut on_bucket = SideBucket::new();
    let mut off_bucket = SideBucket::new();

    loop {
        if state.attached_pid.load(Ordering::Relaxed) != pid {
            break;
        }
        let n = sm_host::drain(ring, &mut scratch);
        let limiter_on = state.limiter_on.load(Ordering::Relaxed);
        for f in &scratch[..n] {
            if let Some(prev) = last_present {
                let ft_us = (f.present_start_qpc - prev) as f64 * 1e6 / freq;
                if ft_us > 0.0 && ft_us < 1_000_000.0 {
                    let t_s = f.present_start_qpc as f64 / freq;
                    // Present call duration (unchanged by the limiter) and the
                    // pacing wait applied AFTER the real Present returned.
                    let hold_us = (f.present_end_qpc - f.present_start_qpc) as f64 * 1e6 / freq;
                    let wait_us = (f.release_qpc - f.present_end_qpc).max(0) as f64 * 1e6 / freq;

                    // Reset on backwards time jump
                    if let Some(&(last_t, _)) = samples.back() {
                        if t_s < last_t {
                            samples.clear();
                        }
                    }
                    samples.push_back((t_s, ft_us));

                    if limiter_on {
                        on_bucket.push(t_s, ft_us, hold_us, wait_us);
                    } else {
                        off_bucket.push(t_s, ft_us, hold_us, wait_us);
                    }
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
            while let Some(&(front_t, _)) = samples.front() {
                if front_t < cutoff || samples.len() > MAX_QUEUE_SAMPLES {
                    samples.pop_front();
                } else {
                    break;
                }
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
            stats.on = on_bucket.publish();
            stats.off = off_bucket.publish();
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dq(v: &[(f64, f64)]) -> VecDeque<(f64, f64)> {
        v.iter().copied().collect()
    }

    #[test]
    fn side_stats_empty_and_single_are_zero() {
        assert_eq!(compute_side_stats(&VecDeque::new()), (0.0, 0.0, 0.0));
        assert_eq!(compute_side_stats(&dq(&[(1.0, 5000.0)])), (0.0, 0.0, 0.0));
    }

    #[test]
    fn side_stats_constant_frametime_is_flat() {
        let s = dq(&[(0.0, 16_666.667), (0.1, 16_666.667), (0.2, 16_666.667)]);
        let (fps, p50, p99) = compute_side_stats(&s);
        assert!((fps - 60.0).abs() < 0.01, "fps {fps}");
        assert!((p50 - 16_666.667).abs() < 0.01, "p50 {p50}");
        assert!((p99 - 16_666.667).abs() < 0.01, "p99 {p99}");
    }

    #[test]
    fn side_stats_p99_catches_outliers() {
        // 200 normal frames + 10 spikes → p99 must land on the spike value.
        let mut v: Vec<(f64, f64)> = (0..200).map(|i| (i as f64, 10_000.0)).collect();
        v.extend((200..210).map(|i| (i as f64, 50_000.0)));
        let (_, _, p99) = compute_side_stats(&dq(&v));
        assert!((p99 - 50_000.0).abs() < 1e-9, "p99 {p99}");
    }

    #[test]
    fn side_bucket_trims_to_window_and_freezes_when_inactive() {
        let mut b = SideBucket::new();
        // 15 s of samples at 10 Hz → only the last 10 s survive the trim.
        for i in 0..150 {
            b.push(i as f64 * 0.1, 10_000.0, 120.0, 5.0);
        }
        assert_eq!(b.samples.len(), 101);
        assert_eq!(b.holds.len(), 101);
        assert_eq!(b.total, 150);
        let published = b.publish();
        assert_eq!(published.total, 150);
        assert!(published.has_timing);
        assert!((published.fps - 100.0).abs() < 0.01);
        assert!((published.hold_p50_us - 120.0).abs() < 1e-9);
        assert!((published.wait_p50_us - 5.0).abs() < 1e-9);
    }
}