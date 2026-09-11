//! High-Frequency Raw Input & Mouse Delta Tracking for Asynchronous Camera Warping.

use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

/// Monotonic QueryPerformanceCounter timestamp.
#[inline]
pub fn qpc_now() -> i64 {
    use windows::Win32::System::Performance::QueryPerformanceCounter;
    let mut v = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}

/// QueryPerformanceFrequency (ticks per second).
pub fn qpc_frequency() -> i64 {
    use windows::Win32::System::Performance::QueryPerformanceFrequency;
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 { 10_000_000 } else { f }
}

/// Atomic accumulator for high-rate mouse deltas (1000 Hz Raw Input).
#[repr(C, align(64))]
pub struct InputAccumulator {
    /// Accumulated horizontal mouse delta (pixels / counts).
    acc_dx: AtomicI32,
    /// Accumulated vertical mouse delta (pixels / counts).
    acc_dy: AtomicI32,
    /// Timestamp of the last received mouse event.
    last_event_qpc: AtomicI64,
}

impl Default for InputAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl InputAccumulator {
    pub const fn new() -> Self {
        Self {
            acc_dx: AtomicI32::new(0),
            acc_dy: AtomicI32::new(0),
            last_event_qpc: AtomicI64::new(0),
        }
    }

    /// Feeds an incoming RawInput mouse delta into the accumulator.
    #[inline]
    pub fn push_delta(&self, dx: i32, dy: i32) {
        self.acc_dx.fetch_add(dx, Ordering::Relaxed);
        self.acc_dy.fetch_add(dy, Ordering::Relaxed);
        self.last_event_qpc.store(qpc_now(), Ordering::Release);
    }

    /// Samples and resets the accumulated mouse delta since the last frame.
    #[inline]
    pub fn sample_and_reset(&self) -> (i32, i32, i64) {
        let dx = self.acc_dx.swap(0, Ordering::AcqRel);
        let dy = self.acc_dy.swap(0, Ordering::AcqRel);
        let ts = self.last_event_qpc.load(Ordering::Acquire);
        (dx, dy, ts)
    }

    /// Inspects the currently accumulated mouse delta without resetting.
    #[inline]
    pub fn peek(&self) -> (i32, i32) {
        (
            self.acc_dx.load(Ordering::Relaxed),
            self.acc_dy.load(Ordering::Relaxed),
        )
    }
}

/// Converts raw pixel delta into camera angles (yaw and pitch in radians).
#[inline]
pub fn delta_to_angles(dx: i32, dy: i32, yaw_sensitivity: f32, pitch_sensitivity: f32) -> (f32, f32) {
    let yaw = dx as f32 * yaw_sensitivity;
    let pitch = dy as f32 * pitch_sensitivity;
    (yaw, pitch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_push_and_sample() {
        let acc = InputAccumulator::new();
        acc.push_delta(15, -8);
        acc.push_delta(5, 3);
        let (dx, dy, _ts) = acc.sample_and_reset();
        assert_eq!(dx, 20);
        assert_eq!(dy, -5);

        // After reset, next sample must be 0
        let (dx2, dy2, _) = acc.sample_and_reset();
        assert_eq!(dx2, 0);
        assert_eq!(dy2, 0);
    }
}
