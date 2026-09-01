//! Vulkan hooks: `vkQueuePresentKHR`, `vkGetDeviceProcAddr`, `vkGetInstanceProcAddr`.
//!
//! Architecture:
//! 1. When `vulkan-1.dll` is present in the process, we locate its exported
//!    functions (`vkGetInstanceProcAddr`, `vkGetDeviceProcAddr`, `vkQueuePresentKHR`).
//! 2. Modern Vulkan games query device-specific function pointers via `vkGetDeviceProcAddr`.
//!    We hook `vkGetDeviceProcAddr` and `vkGetInstanceProcAddr` so that requests for
//!    `"vkQueuePresentKHR"` return our `hooked_vk_queue_present_khr` wrapper while storing
//!    the driver's real entry point.
//! 3. We also inline-hook the exported `vkQueuePresentKHR` in `vulkan-1.dll` as a fallback.
//! 4. Inside `hooked_vk_queue_present_khr`, we execute the real presentation, capture QPC
//!    timestamps, invoke the JIT pacer engine (`engine::pace`), and emit telemetry.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    PAGE_PROTECTION_FLAGS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::telemetry;

// --- Vulkan Type Definitions ------------------------------------------------

pub type VkResult = i32;
pub const VK_SUCCESS: VkResult = 0;

pub type VkStructureType = i32;

pub type VkInstance = *mut c_void;
pub type VkDevice = *mut c_void;
pub type VkQueue = *mut c_void;
pub type VkSwapchainKHR = u64;

#[repr(C)]
pub struct VkPresentInfoKHR {
    pub s_type: VkStructureType,
    pub p_next: *const c_void,
    pub wait_semaphore_count: u32,
    pub p_wait_semaphores: *const u64,
    pub swapchain_count: u32,
    pub p_swapchains: *const VkSwapchainKHR,
    pub p_image_indices: *const u32,
    pub p_results: *mut VkResult,
}

pub type PfnVkVoidFunction = Option<unsafe extern "system" fn()>;

pub type PfnVkGetInstanceProcAddr = unsafe extern "system" fn(
    instance: VkInstance,
    p_name: *const c_char,
) -> PfnVkVoidFunction;

pub type PfnVkGetDeviceProcAddr = unsafe extern "system" fn(
    device: VkDevice,
    p_name: *const c_char,
) -> PfnVkVoidFunction;

pub type PfnVkQueuePresentKhr = unsafe extern "system" fn(
    queue: VkQueue,
    p_present_info: *const VkPresentInfoKHR,
) -> VkResult;

// --- Static Function Pointers -----------------------------------------------

static ORIG_GET_INSTANCE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_GET_DEVICE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_QUEUE_PRESENT_EXPORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Driver-specific real `vkQueuePresentKHR` retrieved from `vkGetDeviceProcAddr`.
static DRIVER_QUEUE_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

static HOOKED_INSTALLED: AtomicBool = AtomicBool::new(false);

// --- Inline Hook / Detour Utility -------------------------------------------

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

/// Creates an inline hook on `target_fn` jumping to `hook_fn`.
/// Returns the trampoline address (calling this executes the original instructions + continues).
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

// --- Hook Implementations ---------------------------------------------------

unsafe extern "system" fn hooked_vk_queue_present_khr(
    queue: VkQueue,
    p_present_info: *const VkPresentInfoKHR,
) -> VkResult {
    let t_start = crate::timing::qpc_now();

    // 1. Call real driver or exported present
    let driver_ptr = DRIVER_QUEUE_PRESENT.load(Ordering::Acquire);
    let export_ptr = ORIG_QUEUE_PRESENT_EXPORT.load(Ordering::Acquire);

    let res = if !driver_ptr.is_null() {
        let real_fn: PfnVkQueuePresentKhr = std::mem::transmute(driver_ptr);
        real_fn(queue, p_present_info)
    } else if !export_ptr.is_null() {
        let real_fn: PfnVkQueuePresentKhr = std::mem::transmute(export_ptr);
        real_fn(queue, p_present_info)
    } else {
        VK_SUCCESS
    };

    let t_end = crate::timing::qpc_now();

    // 2. Pace and record telemetry
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

unsafe extern "system" fn hooked_vk_get_device_proc_addr(
    device: VkDevice,
    p_name: *const c_char,
) -> PfnVkVoidFunction {
    let orig_ptr = ORIG_GET_DEVICE_PROC_ADDR.load(Ordering::Acquire);
    if orig_ptr.is_null() {
        return None;
    }
    let orig_fn: PfnVkGetDeviceProcAddr = std::mem::transmute(orig_ptr);

    if !p_name.is_null() {
        if let Ok(name) = CStr::from_ptr(p_name).to_str() {
            if name == "vkQueuePresentKHR" {
                let real_present = orig_fn(device, p_name);
                if let Some(real_fn) = real_present {
                    DRIVER_QUEUE_PRESENT.store(real_fn as *mut c_void, Ordering::Release);
                }
                return Some(std::mem::transmute(hooked_vk_queue_present_khr as *const ()));
            }
            if name == "vkGetDeviceProcAddr" {
                return Some(std::mem::transmute(hooked_vk_get_device_proc_addr as *const ()));
            }
        }
    }

    orig_fn(device, p_name)
}

unsafe extern "system" fn hooked_vk_get_instance_proc_addr(
    instance: VkInstance,
    p_name: *const c_char,
) -> PfnVkVoidFunction {
    let orig_ptr = ORIG_GET_INSTANCE_PROC_ADDR.load(Ordering::Acquire);
    if orig_ptr.is_null() {
        return None;
    }
    let orig_fn: PfnVkGetInstanceProcAddr = std::mem::transmute(orig_ptr);

    if !p_name.is_null() {
        if let Ok(name) = CStr::from_ptr(p_name).to_str() {
            if name == "vkQueuePresentKHR" {
                let real_present = orig_fn(instance, p_name);
                if let Some(real_fn) = real_present {
                    DRIVER_QUEUE_PRESENT.store(real_fn as *mut c_void, Ordering::Release);
                }
                return Some(std::mem::transmute(hooked_vk_queue_present_khr as *const ()));
            }
            if name == "vkGetDeviceProcAddr" {
                return Some(std::mem::transmute(hooked_vk_get_device_proc_addr as *const ()));
            }
            if name == "vkGetInstanceProcAddr" {
                return Some(std::mem::transmute(hooked_vk_get_instance_proc_addr as *const ()));
            }
        }
    }

    orig_fn(instance, p_name)
}

// --- Public Hook Management -------------------------------------------------

/// Installs Vulkan hooks if `vulkan-1.dll` is loaded in the process.
/// Fails gracefully if Vulkan is not present (standard for non-Vulkan games).
pub unsafe fn install() -> Result<(), String> {
    if HOOKED_INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }

    let vulkan_mod: HMODULE = unsafe {
        GetModuleHandleW(PCWSTR::from_raw(w!("vulkan-1.dll").as_ptr()))
    }.map_err(|_| "vulkan-1.dll not loaded in process".to_string())?;

    if vulkan_mod.is_invalid() {
        return Err("vulkan-1.dll handle invalid".into());
    }

    crate::log_line("Vulkan module detected: installing hooks...");

    // 1. Hook vkGetInstanceProcAddr
    let get_inst = GetProcAddress(vulkan_mod, s!("vkGetInstanceProcAddr"))
        .ok_or("GetProcAddress(vkGetInstanceProcAddr) failed")?;
    let tramp_inst = make_detour_14(
        get_inst as *mut c_void,
        hooked_vk_get_instance_proc_addr as *mut c_void,
    )?;
    ORIG_GET_INSTANCE_PROC_ADDR.store(tramp_inst, Ordering::Release);

    // 2. Hook vkGetDeviceProcAddr
    let get_dev = GetProcAddress(vulkan_mod, s!("vkGetDeviceProcAddr"))
        .ok_or("GetProcAddress(vkGetDeviceProcAddr) failed")?;
    let tramp_dev = make_detour_14(
        get_dev as *mut c_void,
        hooked_vk_get_device_proc_addr as *mut c_void,
    )?;
    ORIG_GET_DEVICE_PROC_ADDR.store(tramp_dev, Ordering::Release);

    // 3. Opportunistically hook exported vkQueuePresentKHR if exported directly
    if let Some(present) = GetProcAddress(vulkan_mod, s!("vkQueuePresentKHR")) {
        if let Ok(tramp_present) = make_detour_14(
            present as *mut c_void,
            hooked_vk_queue_present_khr as *mut c_void,
        ) {
            ORIG_QUEUE_PRESENT_EXPORT.store(tramp_present, Ordering::Release);
        }
    }

    HOOKED_INSTALLED.store(true, Ordering::Release);
    crate::log_line("Vulkan hooks successfully installed");
    Ok(())
}

/// Uninstalls Vulkan hooks (best-effort).
pub fn uninstall() {
    HOOKED_INSTALLED.store(false, Ordering::Release);
}
