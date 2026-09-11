//! OpenGL hooks: `wglSwapBuffers` in `opengl32.dll` and `SwapBuffers` in `gdi32.dll`.
//!
//! Architecture:
//! 1. When `opengl32.dll` or `gdi32.dll` is loaded, we hook the respective swap buffer entrypoints.
//! 2. Inside the hooks, we execute the real buffer swap, measure QPC timestamps,
//!    call the pacing engine (`engine::pace`), and record telemetry in the shared ring buffer.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{s, w, BOOL, PCWSTR};
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::hooks::detour::make_detour;
use crate::telemetry;

type SwapBuffersFn = unsafe extern "system" fn(hdc: HDC) -> BOOL;

static ORIG_WGL_SWAP_BUFFERS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_GDI_SWAP_BUFFERS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static HOOKED_INSTALLED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn hooked_wgl_swap_buffers(hdc: HDC) -> BOOL {
    let t_start = crate::timing::qpc_now();

    let orig_ptr = ORIG_WGL_SWAP_BUFFERS.load(Ordering::Acquire);
    let res = if !orig_ptr.is_null() {
        let orig_fn: SwapBuffersFn = std::mem::transmute(orig_ptr);
        orig_fn(hdc)
    } else {
        BOOL(1)
    };

    let t_end = crate::timing::qpc_now();

    let cfg = telemetry::config();
    if cfg.enabled {
        let hint = crate::vblank::hint();
        if let Some((release, decision)) = crate::engine::pace(t_start, t_end, &cfg, hint) {
            telemetry::record_paced(t_start, t_end, release, &decision, false);
            return res;
        }
    }
    telemetry::record_present(t_start, t_end);

    res
}

unsafe extern "system" fn hooked_gdi_swap_buffers(hdc: HDC) -> BOOL {
    let t_start = crate::timing::qpc_now();

    let orig_ptr = ORIG_GDI_SWAP_BUFFERS.load(Ordering::Acquire);
    let res = if !orig_ptr.is_null() {
        let orig_fn: SwapBuffersFn = std::mem::transmute(orig_ptr);
        orig_fn(hdc)
    } else {
        BOOL(1)
    };

    let t_end = crate::timing::qpc_now();

    let cfg = telemetry::config();
    if cfg.enabled {
        let hint = crate::vblank::hint();
        if let Some((release, decision)) = crate::engine::pace(t_start, t_end, &cfg, hint) {
            telemetry::record_paced(t_start, t_end, release, &decision, false);
            return res;
        }
    }
    telemetry::record_present(t_start, t_end);

    res
}

/// Installs OpenGL hooks if `opengl32.dll` or `gdi32.dll` is loaded.
pub unsafe fn install() -> Result<(), String> {
    if HOOKED_INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }

    let mut hooked_any = false;

    // 1. Hook opengl32.dll!wglSwapBuffers
    if let Ok(gl_mod) = GetModuleHandleW(PCWSTR::from_raw(w!("opengl32.dll").as_ptr())) {
        if !gl_mod.is_invalid() {
            if let Some(wgl_swap) = GetProcAddress(gl_mod, s!("wglSwapBuffers")) {
                if let Ok(tramp) = make_detour(
                    wgl_swap as *mut c_void,
                    hooked_wgl_swap_buffers as *mut c_void,
                ) {
                    ORIG_WGL_SWAP_BUFFERS.store(tramp, Ordering::Release);
                    hooked_any = true;
                    crate::log_line("wglSwapBuffers hooked successfully");
                }
            }
        }
    }

    // 2. Hook gdi32.dll!SwapBuffers
    if let Ok(gdi_mod) = GetModuleHandleW(PCWSTR::from_raw(w!("gdi32.dll").as_ptr())) {
        if !gdi_mod.is_invalid() {
            if let Some(gdi_swap) = GetProcAddress(gdi_mod, s!("SwapBuffers")) {
                if let Ok(tramp) = make_detour(
                    gdi_swap as *mut c_void,
                    hooked_gdi_swap_buffers as *mut c_void,
                ) {
                    ORIG_GDI_SWAP_BUFFERS.store(tramp, Ordering::Release);
                    hooked_any = true;
                    crate::log_line("gdi32 SwapBuffers hooked successfully");
                }
            }
        }
    }

    if hooked_any {
        HOOKED_INSTALLED.store(true, Ordering::Release);
        Ok(())
    } else {
        Err("neither opengl32.dll nor gdi32.dll swap functions could be hooked".into())
    }
}

pub fn uninstall() {
    HOOKED_INSTALLED.store(false, Ordering::Release);
}
