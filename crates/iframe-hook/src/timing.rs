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
    while qpc_now() < target_qpc {
        std::hint::spin_loop();
    }
}

/// Hybrid high-precision wait: `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` for the
/// bulk of the wait, QPC spin-wait for the last ~400 µs (sub-10 µs accuracy).
pub struct HighResSleeper {
    timer: HANDLE,
}

impl HighResSleeper {
    pub fn new() -> Option<Self> {
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
        .ok()?;
        Some(Self { timer: handle })
    }

    /// Sleep until `target_qpc`. Returns immediately if that time has passed.
    pub fn sleep_until(&self, target_qpc: i64) {
        use windows::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject};
        let freq = qpc_frequency();
        loop {
            let now = qpc_now();
            let remaining_us = (target_qpc - now) as f64 * 1_000_000.0 / freq as f64;
            if remaining_us <= 0.0 {
                return;
            }
            if remaining_us > 600.0 {
                // Kernel wait for the bulk; keep ~400 µs for the spin.
                let kernel_us = remaining_us - 400.0;
                let due_100ns = -((kernel_us * 10.0) as i64); // negative = relative
                unsafe {
                    let _ = SetWaitableTimer(self.timer, &due_100ns, 0, None, None, false);
                    let _ = WaitForSingleObject(self.timer, u32::MAX);
                }
            }
            // Final approach: spin on QPC.
            spin_until(target_qpc);
            return;
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