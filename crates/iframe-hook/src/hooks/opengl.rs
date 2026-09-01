//! OpenGL hooks: `wglSwapBuffers` in `opengl32.dll`.
//!
//! Architecture:
//! 1. When `opengl32.dll` is loaded, we hook `wglSwapBuffers`.
//! 2. Inside `hooked_wgl_swap_buffers`, we execute the real buffer swap, measure QPC timestamps,
//!    call the pacing engine (`engine::pace`), and record telemetry in the shared ring buffer.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{s, w, BOOL, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    PAGE_PROTECTION_FLAGS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::telemetry;

type WglSwapBuffersFn = unsafe extern "system" fn(hdc: HDC) -> BOOL;

static ORIG_WGL_SWAP_BUFFERS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static HOOKED_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(target_pointer_width = "64")]
#[repr(C, packed)]
struct Jmp14 {
    opcode: [u8; 6], // FF 25 00 00 00 00 = jmp qword ptr [rip + 0]
    target: u64,
}

#[cfg(target_pointer_width = "32")]
#[repr(C, packed)]
struct Jmp5 {
    opcode: u8, // 0xE9 = jmp rel32
    target: i32,
}

pub unsafe fn make_detour_14(target_fn: *mut c_void, hook_fn: *mut c_void) -> Result<*mut c_void, String> {
    if target_fn.is_null() || hook_fn.is_null() {
        return Err("null pointer passed to make_detour".into());
    }

    #[cfg(target_pointer_width = "64")]
    {
        const DETOUR_LEN: usize = 14;
        const TRAMPOLINE_STUB_LEN: usize = 32;

        let tramp_mem = VirtualAlloc(
            None,
            TRAMPOLINE_STUB_LEN,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        );
        if tramp_mem.is_null() {
            return Err("VirtualAlloc for trampoline failed".into());
        }

        std::ptr::copy_nonoverlapping(target_fn as *const u8, tramp_mem as *mut u8, DETOUR_LEN);

        let jump_back = tramp_mem.add(DETOUR_LEN) as *mut Jmp14;
        std::ptr::write_unaligned(
            jump_back,
            Jmp14 {
                opcode: [0xFF, 0x25, 0x00, 0x00, 0x00, 0x00],
                target: (target_fn as usize + DETOUR_LEN) as u64,
            },
        );

        let mut old_protect = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(target_fn, DETOUR_LEN, PAGE_EXECUTE_READWRITE, &mut old_protect)
            .map_err(|e| format!("VirtualProtect target: {e}"))?;

        let jmp_to_hook = target_fn as *mut Jmp14;
        std::ptr::write_unaligned(
            jmp_to_hook,
            Jmp14 {
                opcode: [0xFF, 0x25, 0x00, 0x00, 0x00, 0x00],
                target: hook_fn as usize as u64,
            },
        );

        VirtualProtect(target_fn, DETOUR_LEN, old_protect, &mut old_protect)
            .map_err(|e| format!("VirtualProtect restore: {e}"))?;

        let _ = FlushInstructionCache(GetCurrentProcess(), Some(target_fn), DETOUR_LEN);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(tramp_mem), TRAMPOLINE_STUB_LEN);

        Ok(tramp_mem)
    }

    #[cfg(target_pointer_width = "32")]
    {
        const DETOUR_LEN: usize = 5;
        const TRAMPOLINE_STUB_LEN: usize = 16;

        let tramp_mem = VirtualAlloc(
            None,
            TRAMPOLINE_STUB_LEN,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        );
        if tramp_mem.is_null() {
            return Err("VirtualAlloc for trampoline failed".into());
        }

        std::ptr::copy_nonoverlapping(target_fn as *const u8, tramp_mem as *mut u8, DETOUR_LEN);

        let jump_back = tramp_mem.add(DETOUR_LEN) as *mut Jmp5;
        let tramp_back_rel = (target_fn as isize + DETOUR_LEN as isize) - (jump_back as isize + 5);
        std::ptr::write_unaligned(
            jump_back,
            Jmp5 {
                opcode: 0xE9,
                target: tramp_back_rel as i32,
            },
        );

        let mut old_protect = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(target_fn, DETOUR_LEN, PAGE_EXECUTE_READWRITE, &mut old_protect)
            .map_err(|e| format!("VirtualProtect target: {e}"))?;

        let jmp_to_hook = target_fn as *mut Jmp5;
        let target_rel = (hook_fn as isize) - (target_fn as isize + 5);
        std::ptr::write_unaligned(
            jmp_to_hook,
            Jmp5 {
                opcode: 0xE9,
                target: target_rel as i32,
            },
        );

        VirtualProtect(target_fn, DETOUR_LEN, old_protect, &mut old_protect)
            .map_err(|e| format!("VirtualProtect restore: {e}"))?;

        let _ = FlushInstructionCache(GetCurrentProcess(), Some(target_fn), DETOUR_LEN);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(tramp_mem), TRAMPOLINE_STUB_LEN);

        Ok(tramp_mem)
    }
}

unsafe extern "system" fn hooked_wgl_swap_buffers(hdc: HDC) -> BOOL {
    let t_start = crate::timing::qpc_now();

    let orig_ptr = ORIG_WGL_SWAP_BUFFERS.load(Ordering::Acquire);
    let res = if !orig_ptr.is_null() {
        let orig_fn: WglSwapBuffersFn = std::mem::transmute(orig_ptr);
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

/// Installs OpenGL hooks if `opengl32.dll` is loaded.
pub unsafe fn install() -> Result<(), String> {
    if HOOKED_INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }

    let gl_mod_res: Result<HMODULE, _> = unsafe {
        GetModuleHandleW(PCWSTR::from_raw(w!("opengl32.dll").as_ptr()))
    };
    let gl_mod = gl_mod_res.map_err(|_| "opengl32.dll not loaded".to_string())?;
    if gl_mod.is_invalid() {
        return Err("opengl32.dll handle invalid".into());
    }

    let wgl_swap = GetProcAddress(gl_mod, s!("wglSwapBuffers"))
        .ok_or("GetProcAddress(wglSwapBuffers) failed")?;
    let tramp = make_detour_14(
        wgl_swap as *mut c_void,
        hooked_wgl_swap_buffers as *mut c_void,
    )?;
    ORIG_WGL_SWAP_BUFFERS.store(tramp, Ordering::Release);
    HOOKED_INSTALLED.store(true, Ordering::Release);
    crate::log_line("wglSwapBuffers hooked successfully");
    Ok(())
}

pub fn uninstall() {
    HOOKED_INSTALLED.store(false, Ordering::Release);
}
