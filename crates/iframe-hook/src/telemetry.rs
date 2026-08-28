//! Telemetry shared memory on the DLL side: attach to the mapping created by
//! the control app (`Local\iFrameSM_<pid>`), or create it ourselves when the
//! DLL is loaded standalone (tests / manual LoadLibrary).

use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerDecision;
use iframe_common::shared_mem::{total_size, SharedRing, TelemetryFrame};

/// Ring capacity in records (power of two). 4096 × 48 B ≈ 197 KiB.
pub const RING_CAPACITY: usize = 4096;

static RING: OnceLock<SharedRing> = OnceLock::new();
static VIEW: OnceLock<Mapping> = OnceLock::new();

/// Owning handle for the mapped view (kept alive for the process lifetime).
struct Mapping {
    handle: HANDLE,
    ptr: *mut core::ffi::c_void,
}

unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr });
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Attach to the app-created mapping, or create+initialise one when the DLL
/// is loaded standalone (no control app running).
pub fn init() -> bool {
    let pid = unsafe { GetCurrentProcessId() };
    let name = format!("{}{}\0", iframe_common::SM_NAME_PREFIX, pid)
        .encode_utf16()
        .collect::<Vec<u16>>();
    unsafe {
        // 1) The control app should have created the mapping before injection.
        if let Ok(handle) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr())) {
            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if !view.Value.is_null() {
                if let Some(ring) =
                    SharedRing::attach(view.Value as *mut u8, total_size(RING_CAPACITY))
                {
                    let _ = VIEW.set(Mapping { handle, ptr: view.Value });
                    return RING.set(ring).is_ok();
                }
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
                return false;
            }
            let _ = CloseHandle(handle);
        }

        // 2) Standalone mode: create the mapping ourselves.
        let Ok(handle) = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            total_size(RING_CAPACITY) as u32,
            PCWSTR(name.as_ptr()),
        ) else {
            return false;
        };
        // If the mapping already existed we must not re-initialise it.
        let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        if view.Value.is_null() {
            let _ = CloseHandle(handle);
            return false;
        }
        let ring = if already_exists {
            SharedRing::attach(view.Value as *mut u8, total_size(RING_CAPACITY))
        } else {
            SharedRing::init(view.Value as *mut u8, total_size(RING_CAPACITY), RING_CAPACITY)
        };
        match ring {
            Some(ring) => {
                let _ = VIEW.set(Mapping { handle, ptr: view.Value });
                RING.set(ring).is_ok()
            }
            None => {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
                false
            }
        }
    }
}

/// Current runtime config as published by the control app (few atomic loads).
#[inline]
pub fn config() -> RuntimeConfig {
    ring().map(|r| r.config()).unwrap_or_default()
}

/// Push one pass-through Present record (limiter inert).
#[inline]
pub fn record_present(present_start_qpc: i64, present_end_qpc: i64) {
    if let Some(ring) = RING.get() {
        let frame = TelemetryFrame {
            present_start_qpc,
            present_end_qpc,
            release_qpc: present_end_qpc,
            target_tick_qpc: 0,
            ema_duration_us: 0.0,
            deviation_us: 0.0,
            flags: TelemetryFrame::FLAG_BYPASS,
            _pad: 0,
        };
        ring.push(&frame);
    }
}

/// Push one paced Present record with the pacer's stats.
#[inline]
pub fn record_paced(
    present_start_qpc: i64,
    present_end_qpc: i64,
    release_qpc: i64,
    decision: &PacerDecision,
    vsync_overridden: bool,
) {
    if let Some(ring) = RING.get() {
        let mut flags = 0;
        if decision.stats.bypass {
            flags |= TelemetryFrame::FLAG_BYPASS;
        }
        if decision.stats.late {
            flags |= TelemetryFrame::FLAG_LATE;
        }
        if vsync_overridden {
            flags |= TelemetryFrame::FLAG_VSYNC_OVERRIDE;
        }
        let frame = TelemetryFrame {
            present_start_qpc,
            present_end_qpc,
            release_qpc,
            target_tick_qpc: decision.target_tick_qpc,
            ema_duration_us: decision.stats.ema_duration_us as f32,
            deviation_us: decision.stats.deviation_us as f32,
            flags,
            _pad: 0,
        };
        ring.push(&frame);
    }
}

pub fn ring() -> Option<&'static SharedRing> {
    RING.get()
}