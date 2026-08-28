//! DXGI `IDXGISwapChain::Present` / `Present1` vtable hooks.
//!
//! Technique (RTSS-style): create a dummy D3D11 device + swap chain to obtain
//! the live vtable addresses of `IDXGISwapChain` (slot 8 = Present) and
//! `IDXGISwapChain1` (slot 21 = Present1), then patch those vtable slots.
//! The vtable lives in dxgi.dll's `.rdata`; patching it makes a private
//! copy-on-write page for this process, so every swap chain in the game gets
//! hooked — including ones created before injection.
//!
//! The original function pointers are kept as trampolines and called directly.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, HMODULE, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_MODE_SCALING_UNSPECIFIED,
    DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{VirtualProtect, PAGE_PROTECTION_FLAGS, PAGE_READWRITE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, SW_HIDE, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use crate::telemetry;

// COM vtable slot indices for IDXGISwapChain / IDXGISwapChain1.
// IUnknown(0..2) + IDXGIObject(3..6) + IDXGIDeviceSubObject(7) + IDXGISwapChain(8..17)
// IDXGISwapChain1 extends: GetDesc1(17) GetFullscreenDesc(18) GetHwnd(19)
// GetCoreWindow(20) Present1(21).
const SLOT_PRESENT: usize = 8;
const SLOT_PRESENT1: usize = 21;

type PresentFn = unsafe extern "system" fn(this: *mut c_void, sync_interval: u32, flags: u32) -> i32;
type Present1Fn = unsafe extern "system" fn(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    dirty_rects: *const RECT,
    dirty_rects_count: u32,
) -> i32;

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_PRESENT1: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// The swap chain whose presents are recorded (first one seen wins; a game
/// may own several, and the SPSC ring has a single producer).
static ACTIVE_SWAPCHAIN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Vtable addresses kept for uninstall (restore originals).
static VTABLE_PRESENT: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE_PRESENT1: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Install the DXGI hooks. Idempotent (a second DLL load is a no-op).
pub unsafe fn install() -> Result<(), String> {
    if !ORIG_PRESENT.load(Ordering::Acquire).is_null() {
        return Ok(());
    }

    let hwnd = create_hidden_window().map_err(|e| format!("dummy window: {e}"))?;
    let swapchain = create_dummy_swapchain(hwnd).map_err(|e| format!("dummy swapchain: {e}"))?;

    // IDXGISwapChain vtable → Present (slot 8).
    let vtable = *(swapchain.as_raw() as *mut *mut *mut c_void);
    let orig_present = patch_vtable_slot(vtable, SLOT_PRESENT, hooked_present as *mut c_void)?;

    // IDXGISwapChain1 vtable → Present1 (slot 21).
    let sc1: IDXGISwapChain1 = swapchain.cast().map_err(|e| e.to_string())?;
    let vtable1 = *(sc1.as_raw() as *mut *mut *mut c_void);
    let orig_present1 = patch_vtable_slot(vtable1, SLOT_PRESENT1, hooked_present1 as *mut c_void)?;

    ORIG_PRESENT.store(orig_present, Ordering::Release);
    ORIG_PRESENT1.store(orig_present1, Ordering::Release);
    VTABLE_PRESENT.store(vtable, Ordering::Release);
    VTABLE_PRESENT1.store(vtable1, Ordering::Release);

    // Release COM references; the vtable patch persists (COW page).
    drop(sc1);
    drop(swapchain);
    let _ = DestroyWindow(hwnd);
    crate::log_line("dxgi hooks installed (Present@8, Present1@21)");
    Ok(())
}

pub fn uninstall() {
    let vt = VTABLE_PRESENT.load(Ordering::Acquire);
    if !vt.is_null() {
        let orig = ORIG_PRESENT.load(Ordering::Acquire);
        unsafe { restore_vtable(vt, SLOT_PRESENT, orig) };
        VTABLE_PRESENT.store(std::ptr::null_mut(), Ordering::Release);
    }
    let vt1 = VTABLE_PRESENT1.load(Ordering::Acquire);
    if !vt1.is_null() {
        let orig = ORIG_PRESENT1.load(Ordering::Acquire);
        unsafe { restore_vtable(vt1, SLOT_PRESENT1, orig) };
        VTABLE_PRESENT1.store(std::ptr::null_mut(), Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Hook bodies (hot path — lock-free, allocation-free, panic-free)
// ---------------------------------------------------------------------------

/// Pass-through Present with telemetry. M2 will insert the JIT sleep here,
/// strictly AFTER the real Present has returned.
unsafe extern "system" fn hooked_present(this: *mut c_void, sync_interval: u32, flags: u32) -> i32 {
    let t_start = crate::timing::qpc_now();
    let orig = ORIG_PRESENT.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32 // E_FAIL — trampoline missing, should not happen
    } else {
        (std::mem::transmute::<*mut c_void, PresentFn>(orig))(this, sync_interval, flags)
    };
    let t_end = crate::timing::qpc_now();
    if is_active_swapchain(this) {
        telemetry::record_present(t_start, t_end);
    }
    hr
}

unsafe extern "system" fn hooked_present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    dirty_rects: *const RECT,
    dirty_rects_count: u32,
) -> i32 {
    let t_start = crate::timing::qpc_now();
    let orig = ORIG_PRESENT1.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, Present1Fn>(orig))(
            this,
            sync_interval,
            flags,
            dirty_rects,
            dirty_rects_count,
        )
    };
    let t_end = crate::timing::qpc_now();
    if is_active_swapchain(this) {
        telemetry::record_present(t_start, t_end);
    }
    hr
}

#[inline]
fn is_active_swapchain(this: *mut c_void) -> bool {
    if ACTIVE_SWAPCHAIN.load(Ordering::Relaxed).is_null() {
        let _ = ACTIVE_SWAPCHAIN.compare_exchange(
            std::ptr::null_mut(),
            this,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    ACTIVE_SWAPCHAIN.load(Ordering::Relaxed) == this
}

// ---------------------------------------------------------------------------
// Vtable patching
// ---------------------------------------------------------------------------

/// Overwrite `vtable[index]` with `hook`; returns the original pointer.
unsafe fn patch_vtable_slot(
    vtable: *mut *mut c_void,
    index: usize,
    hook: *mut c_void,
) -> Result<*mut c_void, String> {
    let slot = vtable.add(index);
    let page = (slot as usize) & !0xFFF; // 4 KiB pages on x64 Windows
    let mut old = PAGE_PROTECTION_FLAGS(0);
    VirtualProtect(page as *const c_void, 4096, PAGE_READWRITE, &mut old)
        .map_err(|e| format!("VirtualProtect: {e}"))?;
    let orig = std::ptr::read(slot);
    std::ptr::write(slot, hook);
    // Restore the original protection (best effort).
    let mut tmp = PAGE_PROTECTION_FLAGS(0);
    let _ = VirtualProtect(page as *const c_void, 4096, old, &mut tmp);
    Ok(orig)
}

unsafe fn restore_vtable(vtable: *mut *mut c_void, index: usize, orig: *mut c_void) {
    if vtable.is_null() || orig.is_null() {
        return;
    }
    let slot = vtable.add(index);
    let page = (slot as usize) & !0xFFF;
    let mut old = PAGE_PROTECTION_FLAGS(0);
    if VirtualProtect(page as *const c_void, 4096, PAGE_READWRITE, &mut old).is_ok() {
        std::ptr::write(slot, orig);
        let _ = VirtualProtect(page as *const c_void, 4096, old, &mut old);
    }
}

// ---------------------------------------------------------------------------
// Dummy device / swap chain to obtain vtable addresses
// ---------------------------------------------------------------------------

/// DefWindowProcW is a Rust `unsafe fn` in windows 0.61 — wrap it into a real
/// extern "system" WNDPROC for the WNDCLASSW registration.
unsafe extern "system" fn dummy_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn create_hidden_window() -> Result<HWND, String> {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(dummy_wndproc),
        lpszClassName: w!("iFrameDummyWnd"),
        style: CS_HREDRAW | CS_VREDRAW,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }
    let hinstance = unsafe {
        GetModuleHandleW(PCWSTR::null()).map_err(|e| format!("GetModuleHandleW: {e}"))?
    };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("iFrameDummyWnd"),
            w!("iFrameDummy"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            8,
            8,
            None,
            None,
            Some(windows::Win32::Foundation::HINSTANCE(hinstance.0)),
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    Ok(hwnd)
}

unsafe fn create_dummy_swapchain(hwnd: HWND) -> Result<IDXGISwapChain, String> {
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 8,
            Height: 8,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 0,
                Denominator: 0,
            },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
            Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        OutputWindow: hwnd,
        Windowed: TRUE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        Flags: 0,
    };
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut swapchain: Option<IDXGISwapChain> = None;
    D3D11CreateDeviceAndSwapChain(
        None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_FLAG(0),
        Some(&[D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&desc),
        Some(&mut swapchain),
        Some(&mut device),
        None, // pfeaturelevel out
        Some(&mut context),
    )
    .map_err(|e| format!("D3D11CreateDeviceAndSwapChain: {e}"))?;
    swapchain.ok_or_else(|| "no swap chain returned".into())
}