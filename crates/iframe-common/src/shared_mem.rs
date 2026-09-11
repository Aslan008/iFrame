//! Lock-free SPSC telemetry ring buffer living in shared memory
//! (memory-mapped file `Local\iFrameSM_<pid>`).
//!
//! Layout: `[ SharedHeader ][ record * capacity ]`
//!
//! * Producer = injected DLL (writes one [`TelemetryFrame`] per Present).
//! * Consumer = control app (drains the ring for the graph).
//! * Config   = control app → DLL, published through atomics in the header.
//!
//! All synchronization is done with acquire/release atomics on `head`/`tail`;
//! no locks are ever taken on the game's hot path.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SM_MAGIC: u32 = 0x4946524D; // "IFRM"
pub const SM_VERSION: u32 = 2;

/// One paced frame, as recorded by the hook. 48 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TelemetryFrame {
    /// Game entered Present (frame N complete).
    pub present_start_qpc: i64,
    /// Real Present returned (frame N queued to the display).
    pub present_end_qpc: i64,
    /// Hook released the game thread (frame N+1 starts here).
    pub release_qpc: i64,
    /// Display tick the next frame aims at.
    pub target_tick_qpc: i64,
    pub ema_duration_us: f32,
    pub deviation_us: f32,
    /// Bit 0: bypass · Bit 1: late · Bit 2: vsync overridden to 0.
    pub flags: u32,
    pub _pad: u32,
}

impl TelemetryFrame {
    pub const FLAG_BYPASS: u32 = 1 << 0;
    pub const FLAG_LATE: u32 = 1 << 1;
    pub const FLAG_VSYNC_OVERRIDE: u32 = 1 << 2;

    pub fn set_bypass(&mut self, v: bool) {
        self.flags |= if v { Self::FLAG_BYPASS } else { 0 };
    }
}

/// Runtime configuration published by the app, polled by the DLL each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SharedConfig {
    pub enabled: bool,
    pub mode: crate::pacer::PacerMode,
    pub target_fps: f64,
    pub refresh_hz: f64,
}

/// Header of the shared region. `#[repr(C, align(64))]` keeps atomics on
/// separate cache lines from the record area.
#[repr(C, align(64))]
pub struct SharedHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    /// Ring capacity in records, power of two (stored as `cap - 1` mask).
    pub capacity_mask: AtomicU64,
    /// Written-record counter (producer).
    pub head: AtomicU64,
    /// Read-record counter (consumer).
    pub tail: AtomicU64,
    // ---- config: app -> DLL ----
    pub enabled: AtomicU32,
    pub mode: AtomicU32,
    pub target_fps_bits: AtomicU64,
    pub refresh_hz_bits: AtomicU64,
    // ---- stats: DLL -> app ----
    pub target_pid: AtomicU32,
    pub dropped: AtomicU64,
    /// 0 = initialising, 1 = hooks installed, 2 = init failed.
    pub hook_state: AtomicU32,
    /// К1 toggle published by the app (0 = leave the game's VSync alone).
    pub vsync_override: AtomicU32,
    /// Per-game opt-in: force FRAME_LATENCY_WAITABLE_OBJECT on new swap chains.
    pub force_waitable: AtomicU32,
    /// Host heartbeat timestamp in QPC ticks (written by UI/host every ~20ms).
    pub host_heartbeat_qpc: AtomicU64,
    /// 1 if an interactive host/UI is attached and maintaining heartbeats, 0 for CLI one-shot.
    pub host_present: AtomicU32,
    pub _pad: [u8; 0],
}

/// Total mapping size for a given record capacity (power of two).
pub fn total_size(capacity: usize) -> usize {
    std::mem::size_of::<SharedHeader>() + capacity * std::mem::size_of::<TelemetryFrame>()
}

/// Handle over a shared-memory region. The same memory is mapped into two
/// processes; one side pushes, the other pops (SPSC).
pub struct SharedRing {
    ptr: *mut u8,
    #[allow(dead_code)] // used for validation at attach time
    len: usize,
}

// The ring is shared memory addressed by raw pointer; sending it across
// threads is the entire point (producer thread in DLL, consumer in app).
unsafe impl Send for SharedRing {}
unsafe impl Sync for SharedRing {}

impl SharedRing {
    /// Initialise a freshly mapped (zeroed) region. `capacity` must be a
    /// power of two.
    ///
    /// # Safety
    /// `ptr` must be valid for `len` bytes, 64-byte aligned, and stay alive
    /// as long as the returned ring (and the peer process mapping).
    pub unsafe fn init(ptr: *mut u8, len: usize, capacity: usize) -> Option<Self> {
        if !capacity.is_power_of_two() || len < total_size(capacity) {
            return None;
        }
        let header = unsafe { &*(ptr as *const SharedHeader) };
        header.magic.store(SM_MAGIC, Ordering::Release);
        header.version.store(SM_VERSION, Ordering::Release);
        header
            .capacity_mask
            .store((capacity as u64) - 1, Ordering::Release);
        header.head.store(0, Ordering::Release);
        header.tail.store(0, Ordering::Release);
        header.dropped.store(0, Ordering::Release);
        header.hook_state.store(0, Ordering::Release);
        // Watchdog state MUST be explicitly zeroed: init may be called over
        // non-zeroed memory (heap alloc in tests), and a garbage host_present
        // would make the hook read a garbage heartbeat as a dead host.
        header.host_heartbeat_qpc.store(0, Ordering::Release);
        header.host_present.store(0, Ordering::Release);
        Some(Self { ptr, len })
    }

    /// Hook state as published by the injected DLL (0 booting, 1 ready, 2 failed).
    pub fn hook_state(&self) -> u32 {
        self.header().hook_state.load(Ordering::Acquire)
    }

    /// Producer side: publish the hook state (called by the DLL init thread).
    pub fn set_hook_state(&self, state: u32) {
        self.header().hook_state.store(state, Ordering::Release);
    }

    /// Attach to an already-initialised region (peer process side).
    ///
    /// # Safety
    /// Same contract as [`SharedRing::init`]; the region must have been
    /// initialised by the peer first.
    pub unsafe fn attach(ptr: *mut u8, len: usize) -> Option<Self> {
        let header = unsafe { &*(ptr as *const SharedHeader) };
        if header.magic.load(Ordering::Acquire) != SM_MAGIC {
            return None;
        }
        if header.version.load(Ordering::Acquire) != SM_VERSION {
            return None;
        }
        let mask = header.capacity_mask.load(Ordering::Acquire);
        let cap = (mask + 1) as usize;
        if !cap.is_power_of_two() || len < total_size(cap) {
            return None;
        }
        // Fast-forward tail if the consumer attaches after producer has wrapped
        let head = header.head.load(Ordering::Acquire);
        let tail = header.tail.load(Ordering::Acquire);
        if head > tail + (cap as u64) {
            header.tail.store(head.saturating_sub(cap as u64), Ordering::Release);
        }
        Some(Self { ptr, len })
    }

    fn header(&self) -> &SharedHeader {
        unsafe { &*(self.ptr as *const SharedHeader) }
    }


    fn records_base(&self) -> *mut TelemetryFrame {
        unsafe { self.ptr.add(std::mem::size_of::<SharedHeader>()) as *mut TelemetryFrame }
    }

    fn capacity(&self) -> usize {
        (self.header().capacity_mask.load(Ordering::Acquire) + 1) as usize
    }

    /// Producer side: publish one record. Returns `false` (and counts a drop)
    /// when the consumer lags more than a full ring behind.
    pub fn push(&self, rec: &TelemetryFrame) -> bool {
        let header = self.header();
        let cap = self.capacity();
        if cap == 0 {
            return false;
        }
        let head = header.head.load(Ordering::Acquire);
        let tail = header.tail.load(Ordering::Acquire);
        if head.saturating_sub(tail) >= cap as u64 {
            header.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mask = (cap - 1) as u64;
        let slot = (head & mask) as usize;
        unsafe {
            let dst = self.records_base().add(slot);
            std::ptr::write_unaligned(dst, *rec);
        }
        header.head.store(head + 1, Ordering::Release);
        true
    }

    /// Consumer side: pop the oldest record, if any.
    pub fn pop(&self) -> Option<TelemetryFrame> {
        let header = self.header();
        let cap = self.capacity();
        if cap == 0 {
            return None;
        }
        let head = header.head.load(Ordering::Acquire);
        let mut tail = header.tail.load(Ordering::Acquire);
        let cap_u64 = cap as u64;
        if head > tail + cap_u64 {
            tail = head.saturating_sub(cap_u64);
            header.tail.store(tail, Ordering::Release);
        }
        if tail >= head {
            return None;
        }
        let mask = (cap - 1) as u64;
        let slot = (tail & mask) as usize;
        let rec = unsafe { std::ptr::read_unaligned(self.records_base().add(slot)) };
        header.tail.store(tail + 1, Ordering::Release);
        Some(rec)
    }

    /// Drain up to `out.len()` records into `out`; returns how many were read.
    pub fn drain(&self, out: &mut [TelemetryFrame]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.pop() {
                Some(rec) => {
                    out[n] = rec;
                    n += 1;
                }
                None => break,
            }
        }
        n
    }

    // ---- config accessors (app side writes, DLL side reads) ----

    pub fn set_config(&self, cfg: &crate::config::RuntimeConfig) {
        let header = self.header();
        header
            .target_fps_bits
            .store(cfg.target_fps.to_bits(), Ordering::Release);
        header.refresh_hz_bits.store(cfg.refresh_hz.to_bits(), Ordering::Release);
        header.mode.store(cfg.mode.as_u32(), Ordering::Release);
        header
            .enabled
            .store(cfg.enabled as u32, Ordering::Release);
        header
            .vsync_override
            .store(cfg.vsync_override as u32, Ordering::Release);
        header
            .force_waitable
            .store(cfg.force_waitable as u32, Ordering::Release);
    }

    pub fn config(&self) -> crate::config::RuntimeConfig {
        let header = self.header();
        crate::config::RuntimeConfig {
            enabled: header.enabled.load(Ordering::Acquire) != 0,
            mode: crate::pacer::PacerMode::from_u32(header.mode.load(Ordering::Acquire)),
            target_fps: f64::from_bits(header.target_fps_bits.load(Ordering::Acquire)),
            refresh_hz: f64::from_bits(header.refresh_hz_bits.load(Ordering::Acquire)),
            vsync_override: header.vsync_override.load(Ordering::Acquire) != 0,
            force_waitable: header.force_waitable.load(Ordering::Acquire) != 0,
        }
    }

    pub fn dropped(&self) -> u64 {
        self.header().dropped.load(Ordering::Relaxed)
    }

    /// App/Host side: update heartbeat timestamp and declare host presence.
    pub fn update_heartbeat(&self, now_qpc: u64) {
        let header = self.header();
        header.host_heartbeat_qpc.store(now_qpc, Ordering::Release);
        header.host_present.store(1, Ordering::Release);
    }

    /// Hook/DLL side: check if host is alive. Returns true if host is alive or if running headless.
    pub fn is_host_alive(&self, now_qpc: u64, timeout_qpc: u64) -> bool {
        let header = self.header();
        let present = header.host_present.load(Ordering::Acquire);
        if present == 0 {
            return true; // Headless / CLI one-shot mode: no host watchdog required
        }
        let last = header.host_heartbeat_qpc.load(Ordering::Acquire);
        if now_qpc >= last {
            (now_qpc - last) < timeout_qpc
        } else {
            true // Clock skew safeguard
        }
    }

    /// Host side: declare that no interactive host is present (CLI one-shot
    /// commands that publish a config and exit). Clears a stale `host_present`
    /// left by a previous UI session, otherwise the in-game watchdog would
    /// see a dead host and silently ignore the freshly published config.
    pub fn mark_headless(&self) {
        self.header().host_present.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64-byte aligned heap buffer for tests (Vec<u8> is only 1-aligned).
    struct AlignedBuf {
        ptr: *mut u8,
        layout: std::alloc::Layout,
    }

    impl AlignedBuf {
        fn new(len: usize) -> Self {
            let layout =
                std::alloc::Layout::from_size_align(len, 64).expect("valid layout");
            let ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!ptr.is_null());
            Self { ptr, layout }
        }
    }

    impl Drop for AlignedBuf {
        fn drop(&mut self) {
            unsafe { std::alloc::dealloc(self.ptr, self.layout) };
        }
    }

    fn sample_rec(seq: u64) -> TelemetryFrame {
        TelemetryFrame {
            present_start_qpc: seq as i64 * 100,
            present_end_qpc: seq as i64 * 100 + 10,
            release_qpc: seq as i64 * 100 + 20,
            target_tick_qpc: seq as i64 * 100 + 30,
            ema_duration_us: 5000.0 + seq as f32,
            deviation_us: 12.5,
            flags: (seq % 2) as u32,
            _pad: 0,
        }
    }

    #[test]
    fn init_and_attach_roundtrip() {
        let cap = 1024usize;
        let len = total_size(cap);
        let buf = AlignedBuf::new(len);
        unsafe {
            SharedRing::init(buf.ptr, len, cap).expect("init");
            let ring = SharedRing::attach(buf.ptr, len).expect("attach");
            assert_eq!(ring.capacity(), cap);
        }
    }

    #[test]
    fn push_pop_preserves_order() {
        let cap = 128usize;
        let buf = AlignedBuf::new(total_size(cap));
        unsafe {
            let ring = SharedRing::init(buf.ptr, total_size(cap), cap).unwrap();
            // Push fewer than capacity: all must succeed and keep order.
            for seq in 0..100u64 {
                assert!(ring.push(&sample_rec(seq)), "push {seq} must succeed");
            }
            for seq in 0..100u64 {
                let rec = ring.pop().expect("pop {seq}");
                assert_eq!(rec.present_start_qpc, seq as i64 * 100);
            }
            assert!(ring.pop().is_none());
        }
    }

    #[test]
    fn overflow_drops_oldest_policy_counts_drops() {
        let cap = 16usize;
        let buf = AlignedBuf::new(total_size(cap));
        unsafe {
            let ring = SharedRing::init(buf.ptr, total_size(cap), cap).unwrap();
            // Push far more than capacity without consuming.
            for seq in 0..40u64 {
                let ok = ring.push(&sample_rec(seq));
                assert_eq!(ok, seq < 16, "push {seq}");
            }
            assert_eq!(ring.dropped(), 24);
            // Ring holds the first 16 records in order.
            for seq in 0..16u64 {
                let rec = ring.pop().unwrap();
                assert_eq!(rec.present_start_qpc, seq as i64 * 100);
            }
        }
    }

    #[test]
    fn spsc_two_threads_transfer_10k() {
        let cap = 256usize;
        let buf = AlignedBuf::new(total_size(cap));
        unsafe {
            let ring = SharedRing::init(buf.ptr, total_size(cap), cap).unwrap();
            let ring = std::sync::Arc::new(ring);
            let producer_ring = std::sync::Arc::clone(&ring);
            let producer = std::thread::spawn(move || {
                for seq in 0..10_000u64 {
                    while !producer_ring.push(&sample_rec(seq)) {
                        std::hint::spin_loop();
                    }
                }
            });
            let mut received = 0u64;
            while received < 10_000 {
                if let Some(rec) = ring.pop() {
                    assert_eq!(rec.present_start_qpc, received as i64 * 100);
                    received += 1;
                } else {
                    std::hint::spin_loop();
                }
            }
            producer.join().unwrap();
            assert_eq!(received, 10_000);
        }
    }

    #[test]
    fn config_roundtrip_through_header() {
        let cap = 8usize;
        let buf = AlignedBuf::new(total_size(cap));
        unsafe {
            let ring = SharedRing::init(buf.ptr, total_size(cap), cap).unwrap();
            let cfg = crate::config::RuntimeConfig {
                enabled: true,
                mode: crate::pacer::PacerMode::FixedVsync,
                target_fps: 40.0,
                refresh_hz: 120.0,
                vsync_override: true,
                force_waitable: false,
            };
            ring.set_config(&cfg);
            assert_eq!(ring.config(), cfg);
        }
    }

    #[test]
    fn heartbeat_watchdog_lifecycle() {
        let cap = 8usize;
        let buf = AlignedBuf::new(total_size(cap));
        unsafe {
            let ring = SharedRing::init(buf.ptr, total_size(cap), cap).unwrap();
            // Headless by default: the hook must trust the config indefinitely.
            assert!(ring.is_host_alive(1_000, 100));
            // A live host bumps the heartbeat.
            ring.update_heartbeat(1_000);
            assert!(ring.is_host_alive(1_050, 100));
            assert!(
                !ring.is_host_alive(1_200, 100),
                "stale heartbeat must read as dead"
            );
            // Clock skew (now < last) must not read as dead.
            assert!(ring.is_host_alive(900, 100));
            // CLI one-shot after a UI session: back to headless.
            ring.mark_headless();
            assert!(ring.is_host_alive(9_999, 100));
        }
    }

    // Silence unused warning for the import used in layout assertions.
    const _: () = {
        assert!(std::mem::size_of::<TelemetryFrame>() == 48);
        assert!(std::mem::size_of::<SharedHeader>() % 64 == 0);
    };
}