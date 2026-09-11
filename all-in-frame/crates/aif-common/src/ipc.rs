//! Shared Memory Inter-Process Communication between All In Frame UI and Injected Hook.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use crate::config::WarpConfig;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

pub const AIF_MAGIC: u32 = 0x41494631; // "AIF1"
pub const AIF_VERSION: u32 = 1;
pub const SHMEM_NAME: &str = "Local\\AllInFrameSharedMemory\0";

#[repr(C, align(64))]
pub struct AifSharedHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,

    // App -> Hook Config
    pub enabled: AtomicU32,
    pub yaw_sensitivity_bits: AtomicU32,
    pub pitch_sensitivity_bits: AtomicU32,
    pub fov_degrees_bits: AtomicU32,
    pub is_reverse_z: AtomicU32,
    pub hud_mask_enabled: AtomicU32,
    pub hud_depth_threshold_bits: AtomicU32,
    pub fps_multiplier: AtomicU32,
    pub test_pulse: AtomicU32, // Increment to request a 5-degree test warp pulse
    pub continuous_test_wave: AtomicU32, // 1 = oscillate camera in 3D for visible test
    pub debug_depth: AtomicU32, // 1 = render depth heatmap

    // Hook -> App Telemetry & Status
    pub hook_state: AtomicU32, // 0 = idle, 1 = installed, 2 = failed
    pub depth_found: AtomicU32, // 1 = 3D depth buffer active, 0 = scanning
    pub game_fps_bits: AtomicU32,
    pub warped_fps_bits: AtomicU32,
    pub frames_warped: AtomicU64,
    pub last_frame_qpc: AtomicU64,
}

impl Default for AifSharedHeader {
    fn default() -> Self {
        Self {
            magic: AtomicU32::new(AIF_MAGIC),
            version: AtomicU32::new(AIF_VERSION),
            enabled: AtomicU32::new(1),
            yaw_sensitivity_bits: AtomicU32::new(0.0015f32.to_bits()),
            pitch_sensitivity_bits: AtomicU32::new(0.0015f32.to_bits()),
            fov_degrees_bits: AtomicU32::new(90.0f32.to_bits()),
            is_reverse_z: AtomicU32::new(0),
            hud_mask_enabled: AtomicU32::new(0),
            hud_depth_threshold_bits: AtomicU32::new(0.005f32.to_bits()),
            fps_multiplier: AtomicU32::new(1),
            test_pulse: AtomicU32::new(0),
            continuous_test_wave: AtomicU32::new(0),
            debug_depth: AtomicU32::new(0),
            hook_state: AtomicU32::new(0),
            depth_found: AtomicU32::new(0),
            game_fps_bits: AtomicU32::new(0.0f32.to_bits()),
            warped_fps_bits: AtomicU32::new(0.0f32.to_bits()),
            frames_warped: AtomicU64::new(0),
            last_frame_qpc: AtomicU64::new(0),
        }
    }
}

impl AifSharedHeader {
    pub fn publish_config(&self, cfg: &WarpConfig) {
        self.enabled.store(if cfg.enabled { 1 } else { 0 }, Ordering::Release);
        self.yaw_sensitivity_bits.store(cfg.yaw_sensitivity.to_bits(), Ordering::Release);
        self.pitch_sensitivity_bits.store(cfg.pitch_sensitivity.to_bits(), Ordering::Release);
        self.fov_degrees_bits.store(cfg.fov_degrees.to_bits(), Ordering::Release);
        self.is_reverse_z.store(if cfg.is_reverse_z { 1 } else { 0 }, Ordering::Release);
        self.hud_mask_enabled.store(if cfg.hud_mask_enabled { 1 } else { 0 }, Ordering::Release);
        self.hud_depth_threshold_bits.store(cfg.hud_depth_threshold.to_bits(), Ordering::Release);
        self.fps_multiplier.store(cfg.fps_multiplier, Ordering::Release);
        self.continuous_test_wave.store(if cfg.continuous_test_wave { 1 } else { 0 }, Ordering::Release);
        self.test_pulse.store(cfg.test_pulse, Ordering::Release);
        self.debug_depth.store(if cfg.debug_depth { 1 } else { 0 }, Ordering::Release);
    }

    pub fn read_config(&self) -> WarpConfig {
        WarpConfig {
            enabled: self.enabled.load(Ordering::Acquire) != 0,
            yaw_sensitivity: f32::from_bits(self.yaw_sensitivity_bits.load(Ordering::Acquire)),
            pitch_sensitivity: f32::from_bits(self.pitch_sensitivity_bits.load(Ordering::Acquire)),
            fov_degrees: f32::from_bits(self.fov_degrees_bits.load(Ordering::Acquire)),
            depth_near: 0.1,
            depth_far: 1000.0,
            is_reverse_z: self.is_reverse_z.load(Ordering::Acquire) != 0,
            hud_mask_enabled: self.hud_mask_enabled.load(Ordering::Acquire) != 0,
            hud_depth_threshold: f32::from_bits(self.hud_depth_threshold_bits.load(Ordering::Acquire)),
            inpainting_strength: 0.8,
            fps_multiplier: self.fps_multiplier.load(Ordering::Acquire),
            continuous_test_wave: self.continuous_test_wave.load(Ordering::Acquire) != 0,
            test_pulse: self.test_pulse.load(Ordering::Acquire),
            debug_depth: self.debug_depth.load(Ordering::Acquire) != 0,
        }
    }

    pub fn trigger_pulse(&self) {
        self.test_pulse.fetch_add(1, Ordering::SeqCst);
    }
}

pub struct SharedMemoryChannel {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    pub header: *mut AifSharedHeader,
}

unsafe impl Send for SharedMemoryChannel {}
unsafe impl Sync for SharedMemoryChannel {}

impl SharedMemoryChannel {
    pub fn open_or_create() -> Result<Self, String> {
        let name_u16: Vec<u16> = SHMEM_NAME.encode_utf16().collect();
        let len = std::mem::size_of::<AifSharedHeader>() as u32;
        unsafe {
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                len,
                PCWSTR(name_u16.as_ptr()),
            )
            .map_err(|e| format!("CreateFileMappingW: {e}"))?;

            let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return Err("MapViewOfFile failed".into());
            }

            let header = view.Value as *mut AifSharedHeader;
            if !already_exists {
                std::ptr::write(header, AifSharedHeader::default());
            }

            Ok(Self { handle, view, header })
        }
    }

    #[inline]
    pub fn header(&self) -> &AifSharedHeader {
        unsafe { &*self.header }
    }
}

impl Drop for SharedMemoryChannel {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}
