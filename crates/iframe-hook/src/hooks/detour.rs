//! Unified x64/x86 inline hook & detour engine for API intercepts.
//!
//! Provides `make_detour`, allocating an executable trampoline and writing an
//! atomic or protected jump to redirect control flow to our hook function while
//! preserving the ability to call the original implementation.

use std::ffi::c_void;
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    PAGE_PROTECTION_FLAGS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

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

/// Creates an inline detour on `target_fn` jumping to `hook_fn`.
///
/// Returns the trampoline pointer. Calling the trampoline executes the stolen
/// preamble instructions of `target_fn` and jumps back to `target_fn + DETOUR_LEN`.
///
/// # Safety
/// Both `target_fn` and `hook_fn` must be valid executable pointers with appropriate
/// signatures. The first `DETOUR_LEN` bytes of `target_fn` must contain instruction
/// boundaries that can be relocated into a trampoline.
pub unsafe fn make_detour(
    target_fn: *mut c_void,
    hook_fn: *mut c_void,
) -> Result<*mut c_void, String> {
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
