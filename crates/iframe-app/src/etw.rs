//! ETW telemetry-only mode: observe frame presents WITHOUT injecting.
//!
//! A real-time trace session on `Microsoft-Windows-DxgKrnl` listens for
//! Present events (the same source PresentMon uses) and feeds the live
//! frametime graph. Safe for anti-cheat-protected games: read-only kernel
//! observation, no DLL injection, no hooks, nothing an anti-cheat flags.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferrisetw::parser::Parser;
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{TraceTrait, UserTrace};
use ferrisetw::EventRecord;

use crate::live::SharedState;

/// Microsoft-Windows-DxgKrnl — the kernel graphics provider PresentMon uses.
const DXGKRNL_GUID: &str = "802ec45a-1e99-4b83-9966-6c1e23b8d574";

/// Present-family event IDs: 42 = D3D9 Present, 126 = DXGI Present,
/// 168/172/175/184 = flip/queue variants.
const PRESENT_EVENT_IDS: [u16; 6] = [42, 126, 168, 172, 175, 184];

/// A running ETW observation session. Drop (or call `stop`) to end it.
pub struct EtwWatch {
    pub stop_flag: Arc<AtomicBool>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

impl EtwWatch {
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // The session itself is stopped inside the thread via its handle.
    }
}

/// Start observing `pid`'s presents via ETW and feed the live graph.
pub fn start_watch(pid: u32, state: Arc<SharedState>) -> Result<EtwWatch, String> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_cb = stop_flag.clone();
    let trace_name = format!("iFrame-ETW-{pid}");

    let target = Arc::new(AtomicU32::new(pid));
    let last_ts = Arc::new(Mutex::new(0u64));
    let state_cb = state.clone();
    let stop2 = stop_flag.clone();
    let stop3 = stop_flag.clone();
    // Throttle for the A/B + headline stats recompute (worker cadence: 20 ms).
    let last_publish = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60)));

    let provider = Provider::by_guid(DXGKRNL_GUID)
        .add_callback(move |record: &EventRecord, locator: &SchemaLocator| {
            if stop2.load(Ordering::Relaxed) {
                return;
            }
            if !PRESENT_EVENT_IDS.contains(&record.event_id()) {
                return;
            }
            // The kernel logs these events under its own PID — the presenting
            // process is a property of the event, not the record header.
            let Ok(schema) = locator.event_schema(record) else {
                return;
            };
            let parser = Parser::create(record, &schema);
            let Ok(event_pid) = parser.try_parse::<u32>("ProcessId") else {
                return;
            };
            if event_pid != target.load(Ordering::Relaxed) {
                return;
            }

            let ts = record.raw_timestamp() as u64;
            let mut last = last_ts.lock().unwrap();
            let frametime_us = if *last > 0 && ts > *last {
                (ts - *last) as f64 / 10.0 // ETW timestamps are 100 ns units
            } else {
                0.0
            };
            *last = ts;
            drop(last);

            // One stats recompute per 20 ms (matches the SM worker cadence).
            let due = {
                let mut lp = last_publish.lock().unwrap();
                if lp.elapsed() >= Duration::from_millis(20) {
                    *lp = Instant::now();
                    true
                } else {
                    false
                }
            };

            if frametime_us > 0.0 {
                let mut stats = state_cb.stats.lock().unwrap();
                let now_qpc = crate::live::qpc_seconds();
                stats.samples.push_back((now_qpc, frametime_us));
                stats.total += 1;
                // Trim to the history window (10s at up to 500 fps).
                let cutoff = now_qpc - crate::live::HISTORY_SECONDS;
                while let Some(&(t, _)) = stats.samples.front() {
                    if t < cutoff {
                        stats.samples.pop_front();
                    } else {
                        break;
                    }
                }
                // A/B side stats: in ETW mode the limiter can never be on, so
                // every sample belongs to the OFF ("before") side. This also
                // fills the headline FPS/p50/p99 row, which ETW previously left
                // at zero (in inject mode the live.rs worker owns it).
                if due {
                    let (fps, p50, p99) = crate::live::compute_side_stats(&stats.samples);
                    stats.fps = fps;
                    stats.p50_us = p50;
                    stats.p99_us = p99;
                    stats.off = crate::live::SideStats {
                        samples: stats.samples.clone(),
                        fps,
                        p50_us: p50,
                        p99_us: p99,
                        total: stats.total,
                    };
                }
            }
        })
        .build();

    let name = trace_name.clone();
    let thread = std::thread::spawn(move || {
        let trace = UserTrace::new()
            .named(name.clone())
            .enable(provider)
            .start_and_process();
        match trace {
            Ok(mut t) => {
                while !stop3.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                let _ = t.stop();
            }
            Err(e) => {
                // 0x80070005 = access denied — real-time DxgKrnl sessions need
                // an elevated process (same requirement as PresentMon).
                eprintln!(
                    "[iFrame] ETW trace failed: {e:?} — if access was denied, \n[iFrame] run iFrame as administrator (ETW kernel tracing requires elevation)."
                );
            }
        }
    });

    Ok(EtwWatch {
        stop_flag,
        thread: Some(thread),
    })
}