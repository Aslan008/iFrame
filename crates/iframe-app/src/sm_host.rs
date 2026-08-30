//! Shared-memory host side (control app): create/open the mapping the DLL
//! will attach to, and drain telemetry.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

use iframe_common::shared_mem::{total_size, SharedRing, TelemetryFrame};

/// Ring capacity — MUST match the DLL side (telemetry::RING_CAPACITY).
pub const RING_CAPACITY: usize = 4096;

struct View {
    handle: HANDLE,
    ptr: *mut core::ffi::c_void,
}

unsafe impl Send for View {}
unsafe impl Sync for View {}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr });
            let _ = CloseHandle(self.handle);
        }
    }
}

pub struct HostMapping {
    /// Never read directly — held so `Drop` keeps the section mapped.
    #[allow(dead_code)]
    view: View,
    pub ring: SharedRing,
}

unsafe impl Send for HostMapping {}
unsafe impl Sync for HostMapping {}

/// Create (or attach to) the mapping for `pid` and make sure it is
/// initialised before the DLL is injected.
pub fn create_for_pid(pid: u32) -> Result<HostMapping, String> {
    let name = format!("{}{}\0", iframe_common::SM_NAME_PREFIX, pid)
        .encode_utf16()
        .collect::<Vec<u16>>();
    let len = total_size(RING_CAPACITY);
    unsafe {
        let handle = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            len as u32,
            PCWSTR(name.as_ptr()),
        )
        .map_err(|e| format!("CreateFileMappingW: {e}"))?;
        let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        if view.Value.is_null() {
            let _ = CloseHandle(handle);
            return Err("MapViewOfFile failed".into());
        }
        let ring = if already_exists {
            SharedRing::attach(view.Value as *mut u8, len)
        } else {
            SharedRing::init(view.Value as *mut u8, len, RING_CAPACITY)
        };
        match ring {
            Some(ring) => Ok(HostMapping {
                view: View { handle, ptr: view.Value },
                ring,
            }),
            None => {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
                Err("shared ring init/attach failed".into())
            }
        }
    }
}

/// Attach to an existing mapping created by the DLL (standalone mode) or by a
/// previous app session.
pub fn open(pid: u32) -> Result<HostMapping, String> {
    let name = format!("{}{}\0", iframe_common::SM_NAME_PREFIX, pid)
        .encode_utf16()
        .collect::<Vec<u16>>();
    unsafe {
        let handle = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr()))
            .map_err(|e| format!("OpenFileMappingW: {e}"))?;
        let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        if view.Value.is_null() {
            let _ = CloseHandle(handle);
            return Err("MapViewOfFile failed".into());
        }
        let ring = SharedRing::attach(view.Value as *mut u8, total_size(RING_CAPACITY));
        match ring {
            Some(ring) => Ok(HostMapping {
                view: View { handle, ptr: view.Value },
                ring,
            }),
            None => {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
                Err("shared ring attach failed (bad magic/version?)".into())
            }
        }
    }
}

/// Drain helper used by the watcher.
pub fn drain(ring: &SharedRing, out: &mut Vec<TelemetryFrame>) -> usize {
    ring.drain(out)
}