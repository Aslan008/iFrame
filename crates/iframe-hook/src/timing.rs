//! QPC + hybrid high-precision wait for the hook hot path (allocation-free).

use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;

static FREQ: OnceLock<i64> = OnceLock::new();

/// QueryPerformanceCounter frequency (ticks per second), cached.
pub fn qpc_frequency() -> i64 {
    *FREQ.get_or_init(|| {
        use windows::Win32::System::Performance::QueryPerformanceFrequency;
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        if f <= 0 { 10_000_000 } else { f }
    })
}

/// Monotonic high-resolution timestamp in QPC ticks.
#[inline]
pub fn qpc_now() -> i64 {
    use windows::Win32::System::Performance::QueryPerformanceCounter;
    let mut v = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}

/// Spin-wait until `target_qpc` (accuracy < 10 µs, burns CPU — final approach only).
#[inline]
pub fn spin_until(target_qpc: i64) {
    let max_ticks = qpc_frequency() / 10; // 100 ms safety cap against infinite spin
    let deadline = qpc_now().saturating_add(max_ticks).min(target_qpc);
    while qpc_now() < deadline {
        std::hint::spin_loop();
    }
}

/// Hybrid high-precision wait: `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` for the
/// bulk of the wait, QPC spin-wait for the last ~400 µs (sub-10 µs accuracy).
pub struct HighResSleeper {
    timer: HANDLE,
}

// The waitable timer HANDLE is process-wide and used from the present thread;
// the raw handle value carries no thread affinity.
unsafe impl Send for HighResSleeper {}
unsafe impl Sync for HighResSleeper {}

impl HighResSleeper {
    /// Infallible: falls back to spin-only waiting if the high-resolution
    /// timer cannot be created (ancient Windows).
    pub fn new() -> Self {
        use windows::Win32::System::Threading::{
            CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
        };
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .unwrap_or_default();
        Self { timer: handle }
    }

    /// Sleep until `target_qpc`. Returns immediately if that time has passed.
    ///
    /// HARD BOUND: the kernel wait is capped at `remaining + 2 ms` — if the
    /// timer fails to arm we still wake up and spin the rest. A limiter must
    /// NEVER hang the game thread.
    pub fn sleep_until(&self, target_qpc: i64) {
        if self.timer.is_invalid() {
            spin_until(target_qpc);
            return;
        }
        use windows::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject};
        let freq = qpc_frequency();
        let now = qpc_now();
        let remaining_us = (target_qpc - now) as f64 * 1_000_000.0 / freq as f64;
        if remaining_us <= 0.0 {
            return;
        }
        if remaining_us > 600.0 {
            // Kernel wait for the bulk; keep ~400 µs for the spin.
            let kernel_us = remaining_us - 400.0;
            let due_100ns = -((kernel_us * 10.0) as i64); // negative = relative
            // Bounded wait: even if SetWaitableTimer fails or the timer
            // misfires, we wake up and finish with the spin.
            let wait_ms = ((kernel_us / 1000.0).ceil() as u64 + 2).min(u32::MAX as u64) as u32;
            let fired = unsafe {
                SetWaitableTimer(self.timer, &due_100ns, 0, None, None, false).is_ok()
                    && WaitForSingleObject(self.timer, wait_ms)
                        == windows::Win32::Foundation::WAIT_OBJECT_0
            };
            if !fired {
                // Timer misbehaved — spin the remainder (bounded by target).
                spin_until(target_qpc);
                return;
            }
        }
        // Final approach: spin on QPC.
        spin_until(target_qpc);
    }

    /// Sleep capped at `max_us` microseconds — a safety net against a broken
    /// schedule sending the game to sleep for seconds.
    pub fn sleep_until_capped(&self, target_qpc: i64, max_us: f64) {
        let freq = qpc_frequency();
        let now = qpc_now();
        let remaining_us = (target_qpc - now) as f64 * 1_000_000.0 / freq as f64;
        if remaining_us > max_us {
            crate::log_line(&format!(
                "pacing wait capped: requested {remaining_us:.0} µs > {max_us:.0} µs"
            ));
            let capped = now + ((max_us * freq as f64 / 1_000_000.0) as i64);
            self.sleep_until(capped);
        } else {
            self.sleep_until(target_qpc);
        }
    }
}

impl Drop for HighResSleeper {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.timer);
        }
    }
}