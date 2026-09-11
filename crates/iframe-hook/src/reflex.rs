//! NVIDIA Reflex Low Latency API dynamic loader and integration.
//!
//! Provides:
//! 1. Dynamic resolution of `nvapi64.dll` (x64) and `nvapi.dll` (x86) via `nvapi_QueryInterface`.
//! 2. Safe fallback when running on AMD / Intel GPUs (zero panic, zero overhead).
//! 3. Configuration of low latency mode (`NvAPI_D3D_SetSleepMode` with `bLowLatencyMode` and `bLowLatencyBoost`).
//! 4. Frame pacing latency markers (`NvAPI_D3D_SetLatencyMarker`) around Present.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use iframe_common::config::ReflexMode;

// --- NVAPI Function Query IDs ---
const ID_NVAPI_INITIALIZE: u32 = 0x0150E828;
const ID_NVAPI_D3D_SET_SLEEP_MODE: u32 = 0xAC1CA9E0;
const ID_NVAPI_D3D_SET_LATENCY_MARKER: u32 = 0xE6F83861;
const ID_NVAPI_D3D_SLEEP: u32 = 0x852CD1D2;

// --- Reflex Marker Types ---
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyMarkerType {
    SimulationStart = 0,
    SimulationEnd = 1,
    RendersubmitStart = 2,
    RendersubmitEnd = 3,
    PresentStart = 4,
    PresentEnd = 5,
    InputSample = 6,
    TriggerFlash = 7,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvSetSleepModeParams {
    pub version: u32,
    pub low_latency_mode: u8,
    pub low_latency_boost: u8,
    pub use_markers_to_optimize: u8,
    pub _reserved: u8,
    pub minimum_interval_us: u32,
    pub _reserved2: [u32; 4],
}

impl Default for NvSetSleepModeParams {
    fn default() -> Self {
        Self {
            version: std::mem::size_of::<Self>() as u32 | (1 << 16),
            low_latency_mode: 0,
            low_latency_boost: 0,
            use_markers_to_optimize: 1,
            _reserved: 0,
            minimum_interval_us: 0,
            _reserved2: [0; 4],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvLatencyMarkerParams {
    pub version: u32,
    pub frame_id: u64,
    pub marker_type: u32,
    pub _reserved: [u32; 4],
}

impl Default for NvLatencyMarkerParams {
    fn default() -> Self {
        Self {
            version: std::mem::size_of::<Self>() as u32 | (1 << 16),
            frame_id: 0,
            marker_type: 0,
            _reserved: [0; 4],
        }
    }
}

type NvapiQueryInterfaceFn = unsafe extern "C" fn(id: u32) -> *mut c_void;
type NvapiInitializeFn = unsafe extern "C" fn() -> i32;
type NvapiD3dSetSleepModeFn =
    unsafe extern "C" fn(p_dev: *mut c_void, p_params: *const NvSetSleepModeParams) -> i32;
type NvapiD3dSetLatencyMarkerFn =
    unsafe extern "C" fn(p_dev: *mut c_void, p_params: *const NvLatencyMarkerParams) -> i32;
type NvapiD3dSleepFn = unsafe extern "C" fn(p_dev: *mut c_void) -> i32;

pub struct ReflexController {
    available: AtomicBool,
    active_mode: AtomicU32,
    active_fps_bits: std::sync::atomic::AtomicU64,
    fn_set_sleep_mode: AtomicPtr<c_void>,
    fn_set_marker: AtomicPtr<c_void>,
    fn_sleep: AtomicPtr<c_void>,
    last_device: AtomicPtr<c_void>,
}

static INSTANCE: OnceLock<ReflexController> = OnceLock::new();

pub fn get() -> &'static ReflexController {
    INSTANCE.get_or_init(ReflexController::init)
}

impl ReflexController {
    fn init() -> Self {
        let ctrl = Self {
            available: AtomicBool::new(false),
            active_mode: AtomicU32::new(ReflexMode::Off as u32),
            active_fps_bits: std::sync::atomic::AtomicU64::new(0),
            fn_set_sleep_mode: AtomicPtr::new(std::ptr::null_mut()),
            fn_set_marker: AtomicPtr::new(std::ptr::null_mut()),
            fn_sleep: AtomicPtr::new(std::ptr::null_mut()),
            last_device: AtomicPtr::new(std::ptr::null_mut()),
        };

        // Pick 64-bit or 32-bit NVAPI library name
        #[cfg(target_pointer_width = "64")]
        let lib_name = w!("nvapi64.dll");
        #[cfg(target_pointer_width = "32")]
        let lib_name = w!("nvapi.dll");

        let hmod: HMODULE = unsafe {
            match LoadLibraryW(lib_name) {
                Ok(m) => m,
                Err(_) => {
                    crate::log_line("nvapi library not found (non-NVIDIA GPU or missing driver)");
                    return ctrl;
                }
            }
        };

        let qi_proc = unsafe { GetProcAddress(hmod, windows::core::s!("nvapi_QueryInterface")) };
        let Some(qi_proc) = qi_proc else {
            crate::log_line("nvapi_QueryInterface export missing");
            return ctrl;
        };

        let query_interface: NvapiQueryInterfaceFn = unsafe { std::mem::transmute(qi_proc) };

        // Initialize NVAPI
        let init_ptr = unsafe { query_interface(ID_NVAPI_INITIALIZE) };
        if init_ptr.is_null() {
            crate::log_line("nvapi_Initialize interface missing");
            return ctrl;
        }

        let init_fn: NvapiInitializeFn = unsafe { std::mem::transmute(init_ptr) };
        let status = unsafe { init_fn() };
        if status != 0 {
            crate::log_line(&format!("nvapi_Initialize returned status {status}"));
            return ctrl;
        }

        // Query Reflex functions
        let set_sleep_ptr = unsafe { query_interface(ID_NVAPI_D3D_SET_SLEEP_MODE) };
        let set_marker_ptr = unsafe { query_interface(ID_NVAPI_D3D_SET_LATENCY_MARKER) };
        let sleep_ptr = unsafe { query_interface(ID_NVAPI_D3D_SLEEP) };

        if !set_sleep_ptr.is_null() {
            ctrl.fn_set_sleep_mode
                .store(set_sleep_ptr, Ordering::Release);
        }
        if !set_marker_ptr.is_null() {
            ctrl.fn_set_marker.store(set_marker_ptr, Ordering::Release);
        }
        if !sleep_ptr.is_null() {
            ctrl.fn_sleep.store(sleep_ptr, Ordering::Release);
        }

        ctrl.available.store(true, Ordering::Release);
        crate::log_line("NVIDIA Reflex interface successfully initialized");

        // Publish reflex support to shared memory
        if let Some(ring) = crate::telemetry::ring() {
            ring.set_reflex_state(1); // 1 = Supported
        }

        ctrl
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Configure Reflex low latency mode on the given D3D device.
    pub fn configure(&self, device: *mut c_void, mode: ReflexMode, target_fps: f64) {
        if !self.is_available() || device.is_null() {
            return;
        }

        let cur_mode = self.active_mode.load(Ordering::Acquire);
        let cur_dev = self.last_device.load(Ordering::Acquire);
        let cur_fps = self.active_fps_bits.load(Ordering::Acquire);
        if cur_mode == mode as u32 && cur_dev == device && cur_fps == target_fps.to_bits() {
            return;
        }

        let fn_ptr = self.fn_set_sleep_mode.load(Ordering::Acquire);
        if fn_ptr.is_null() {
            return;
        }

        let set_sleep: NvapiD3dSetSleepModeFn = unsafe { std::mem::transmute(fn_ptr) };

        let interval_us = if target_fps > 0.0 && mode != ReflexMode::Off {
            (1_000_000.0 / target_fps) as u32
        } else {
            0
        };

        let params = NvSetSleepModeParams {
            low_latency_mode: if mode != ReflexMode::Off { 1 } else { 0 },
            low_latency_boost: if mode == ReflexMode::Boost { 1 } else { 0 },
            use_markers_to_optimize: 1,
            minimum_interval_us: interval_us,
            ..Default::default()
        };

        let res = unsafe { set_sleep(device, &params) };
        if res == 0 {
            self.active_mode.store(mode as u32, Ordering::Release);
            self.active_fps_bits.store(target_fps.to_bits(), Ordering::Release);
            self.last_device.store(device, Ordering::Release);

            // Publish active status to shared memory
            if let Some(ring) = crate::telemetry::ring() {
                ring.set_reflex_state(if mode != ReflexMode::Off { 2 } else { 1 });
            }
        } else {
            crate::log_line(&format!("NvAPI_D3D_SetSleepMode failed with {res}"));
        }
    }

    /// Place a latency marker in the frame pipeline.
    pub fn set_marker(&self, device: *mut c_void, marker: LatencyMarkerType, frame_id: u64) {
        if !self.is_available() || device.is_null() {
            return;
        }

        let fn_ptr = self.fn_set_marker.load(Ordering::Acquire);
        if fn_ptr.is_null() {
            return;
        }

        let set_marker: NvapiD3dSetLatencyMarkerFn = unsafe { std::mem::transmute(fn_ptr) };
        let params = NvLatencyMarkerParams {
            frame_id,
            marker_type: marker as u32,
            ..Default::default()
        };

        unsafe {
            let _ = set_marker(device, &params);
        }
    }

    /// Trigger NVIDIA driver-managed low latency sleep.
    pub fn sleep(&self, device: *mut c_void) {
        if !self.is_available() || device.is_null() {
            return;
        }

        let fn_ptr = self.fn_sleep.load(Ordering::Acquire);
        if fn_ptr.is_null() {
            return;
        }

        let sleep_fn: NvapiD3dSleepFn = unsafe { std::mem::transmute(fn_ptr) };
        unsafe {
            let _ = sleep_fn(device);
        }
    }
}
