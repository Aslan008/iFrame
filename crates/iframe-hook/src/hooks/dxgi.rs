//! DXGI hooks: `IDXGISwapChain::Present/Present1/ResizeBuffers` + the
//! `IDXGIFactory2` swap-chain creation family.
//!
//! Technique (RTSS-style): create a dummy D3D11 device + swap chain to obtain
//! the live vtable addresses, then patch the vtable slots. The vtables live in
//! dxgi.dll's `.rdata`; patching makes a private copy-on-write page for this
//! process, so every swap chain / factory in the game gets hooked — including
//! ones created before injection.
//!
//! M4 additions:
//! * factory hooks (`CreateSwapChain`, `CreateSwapChainForHwnd/CoreWindow/
//!   Composition`) — the newest swap chain becomes the active one and its
//!   capabilities (windowed, ALLOW_TEARING, waitable object) are cached
//!   straight from the creation desc — zero COM calls;
//! * `ResizeBuffers` hook invalidates the cached caps;
//! * К1 override is tearing-aware: `SyncInterval=0 + ALLOW_TEARING` when the
//!   swap chain opted in, plain `SyncInterval=0` for windowed flip, untouched
//!   for exclusive fullscreen.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, TRUE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGISwapChain, IDXGISwapChain1, IDXGISwapChain2, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, DXGI_PRESENT_PARAMETERS,
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

use iframe_common::pacer::PacerMode;

use crate::telemetry;

// --- COM vtable slot indices ------------------------------------------------

// IDXGISwapChain: IUnknown(0..2) + IDXGIObject(3..6) + IDXGIDeviceSubObject(7)
//   + Present(8) GetBuffer(9) SetFullscreenState(10) GetFullscreenState(11)
//   GetDesc(12) ResizeBuffers(13) ResizeTarget(14) GetContainingOutput(15)
//   GetFrameStatistics(16) GetLastPresentCount(17)
// IDXGISwapChain1 continues: GetDesc1(18) GetFullscreenDesc(19) GetHwnd(20)
//   GetCoreWindow(21) Present1(22) IsTemporaryMonoSupported(23) ...
// (verified against the windows-crate `IDXGISwapChain2_Vtbl` field order AND
//  at runtime by `vtable_slot_indices_match_real_com_objects` below — the old
//  count dropped ResizeTarget and silently hooked GetCoreWindow as Present1)
const SLOT_PRESENT: usize = 8;
const SLOT_RESIZE_BUFFERS: usize = 13;
const SLOT_PRESENT1: usize = 22;

// IDXGIFactory: IUnknown(0..2) + IDXGIObject(3..6) + EnumAdapters(7)
//   MakeWindowAssociation(8) GetWindowAssociation(9) CreateSwapChain(10)
//   CreateSoftwareAdapter(11)
// IDXGIFactory1: EnumAdapters1(12) IsCurrent(13)   <- IsCurrent was dropped in
//   the old count, shifting every IDXGIFactory2 slot down by one
// IDXGIFactory2: IsWindowedStereoEnabled(14) CreateSwapChainForHwnd(15)
//   CreateSwapChainForCoreWindow(16) GetSharedResourceAdapterLuid(17)
//   RegisterStereoStatusWindow(18) RegisterStereoStatusEvent(19)
//   UnregisterStereoStatus(20) RegisterOcclusionStatusWindow(21)
//   RegisterOcclusionStatusEvent(22) UnregisterOcclusionStatus(23)
//   CreateSwapChainForComposition(24)
const SLOT_FACTORY_CREATE: usize = 10;
const SLOT_FACTORY_FOR_HWND: usize = 15;
const SLOT_FACTORY_FOR_CORE_WINDOW: usize = 16;
const SLOT_FACTORY_FOR_COMPOSITION: usize = 24;

type PresentFn = unsafe extern "system" fn(this: *mut c_void, sync_interval: u32, flags: u32) -> i32;
type Present1Fn = unsafe extern "system" fn(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    present_parameters: *const DXGI_PRESENT_PARAMETERS,
) -> i32;
type ResizeBuffersFn =
    unsafe extern "system" fn(this: *mut c_void, u32, u32, u32, u32, u32) -> i32;
type CreateSwapChainFn = unsafe extern "system" fn(
    this: *mut c_void,
    device: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC,
    out: *mut *mut c_void,
) -> i32;
type CreateSwapChainForHwndFn = unsafe extern "system" fn(
    this: *mut c_void,
    device: *mut c_void,
    hwnd: HWND,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    fullscreen_desc: *const windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32;
type CreateSwapChainForCoreWindowFn = unsafe extern "system" fn(
    this: *mut c_void,
    device: *mut c_void,
    window: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32;
type CreateSwapChainForCompositionFn = unsafe extern "system" fn(
    this: *mut c_void,
    device: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32;

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_PRESENT1: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_RESIZE_BUFFERS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_FACTORY_CREATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_FACTORY_FOR_HWND: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_FACTORY_FOR_CORE_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_FACTORY_FOR_COMPOSITION: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The swap chain whose presents are recorded and paced. The newest created
/// swap chain wins (games create their main one last); when the DLL is
/// injected into an already-running game, the first present seen wins.
static ACTIVE_SWAPCHAIN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Vtable addresses kept for uninstall (restore originals).
static VTABLE_PRESENT: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE_PRESENT1: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE_RESIZE: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE_FACTORY: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Capabilities of the active swap chain. Set directly by the creation hooks
/// (from the input desc — no COM calls), invalidated by ResizeBuffers,
/// lazily re-queried via manual vtable calls otherwise.
static CAPS_WINDOWED: AtomicBool = AtomicBool::new(true);
static CAPS_TEARING: AtomicBool = AtomicBool::new(false);
static CAPS_WAITABLE: AtomicBool = AtomicBool::new(false);
static CAPS_VALID: AtomicBool = AtomicBool::new(false);

/// Install the DXGI hooks. Idempotent (a second DLL load is a no-op).
pub unsafe fn install() -> Result<(), String> {
    if !ORIG_PRESENT.load(Ordering::Acquire).is_null() {
        return Ok(());
    }

    let hwnd = create_hidden_window().map_err(|e| format!("dummy window: {e}"))?;
    let (device, swapchain) =
        create_dummy_device_and_swapchain(hwnd).map_err(|e| format!("dummy swapchain: {e}"))?;

    // --- swap chain vtables ---
    let vtable = *(swapchain.as_raw() as *mut *mut *mut c_void);
    let orig_present = patch_vtable_slot(vtable, SLOT_PRESENT, hooked_present as *mut c_void)?;
    let orig_resize =
        patch_vtable_slot(vtable, SLOT_RESIZE_BUFFERS, hooked_resize_buffers as *mut c_void)?;

    let sc1: IDXGISwapChain1 = swapchain.cast().map_err(|e| e.to_string())?;
    let vtable1 = *(sc1.as_raw() as *mut *mut *mut c_void);
    let orig_present1 =
        patch_vtable_slot(vtable1, SLOT_PRESENT1, hooked_present1 as *mut c_void)?;

    ORIG_PRESENT.store(orig_present, Ordering::Release);
    ORIG_PRESENT1.store(orig_present1, Ordering::Release);
    ORIG_RESIZE_BUFFERS.store(orig_resize, Ordering::Release);
    VTABLE_PRESENT.store(vtable, Ordering::Release);
    VTABLE_PRESENT1.store(vtable1, Ordering::Release);
    VTABLE_RESIZE.store(vtable, Ordering::Release);

    // --- factory vtable (CreateSwapChain family) ---
    let dxgi_device: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
    let adapter = dxgi_device.GetAdapter().map_err(|e| e.to_string())?;
    let factory: windows::Win32::Graphics::Dxgi::IDXGIFactory2 =
        adapter.GetParent().map_err(|e| e.to_string())?;
    let factory_vtable = *(factory.as_raw() as *mut *mut *mut c_void);
    let orig_fc = patch_vtable_slot(
        factory_vtable,
        SLOT_FACTORY_CREATE,
        hooked_create_swap_chain as *mut c_void,
    )?;
    let orig_fh = patch_vtable_slot(
        factory_vtable,
        SLOT_FACTORY_FOR_HWND,
        hooked_create_for_hwnd as *mut c_void,
    )?;
    let orig_fcw = patch_vtable_slot(
        factory_vtable,
        SLOT_FACTORY_FOR_CORE_WINDOW,
        hooked_create_for_core_window as *mut c_void,
    )?;
    let orig_fcomp = patch_vtable_slot(
        factory_vtable,
        SLOT_FACTORY_FOR_COMPOSITION,
        hooked_create_for_composition as *mut c_void,
    )?;
    ORIG_FACTORY_CREATE.store(orig_fc, Ordering::Release);
    ORIG_FACTORY_FOR_HWND.store(orig_fh, Ordering::Release);
    ORIG_FACTORY_FOR_CORE_WINDOW.store(orig_fcw, Ordering::Release);
    ORIG_FACTORY_FOR_COMPOSITION.store(orig_fcomp, Ordering::Release);
    VTABLE_FACTORY.store(factory_vtable, Ordering::Release);

    // Release COM references; the vtable patches persist (COW pages).
    drop(factory);
    drop(adapter);
    drop(dxgi_device);
    drop(sc1);
    drop(swapchain);
    drop(device);
    let _ = DestroyWindow(hwnd);

    // Detect ReShade presence for cooperative hook chaining
    let reshade_detected = unsafe {
        GetModuleHandleW(w!("ReShade64.dll")).is_ok()
            || GetModuleHandleW(w!("ReShade32.dll")).is_ok()
            || GetModuleHandleW(w!("ReShade.dll")).is_ok()
    };
    if reshade_detected {
        crate::log_line("ReShade detected in process — chaining hooks cooperatively");
        if let Some(ring) = telemetry::ring() {
            ring.set_reshade_detected(true);
        }
    }

    crate::log_line(
        "dxgi hooks installed (Present@8, ResizeBuffers@13, Present1@22, factory create@10/15/16/24)",
    );
    Ok(())
}

pub fn uninstall() {
    restore_slot(&VTABLE_PRESENT, SLOT_PRESENT, &ORIG_PRESENT);
    restore_slot(&VTABLE_RESIZE, SLOT_RESIZE_BUFFERS, &ORIG_RESIZE_BUFFERS);
    restore_slot(&VTABLE_PRESENT1, SLOT_PRESENT1, &ORIG_PRESENT1);
    restore_slot(&VTABLE_FACTORY, SLOT_FACTORY_CREATE, &ORIG_FACTORY_CREATE);
    restore_slot(&VTABLE_FACTORY, SLOT_FACTORY_FOR_HWND, &ORIG_FACTORY_FOR_HWND);
    restore_slot(
        &VTABLE_FACTORY,
        SLOT_FACTORY_FOR_CORE_WINDOW,
        &ORIG_FACTORY_FOR_CORE_WINDOW,
    );
    restore_slot(
        &VTABLE_FACTORY,
        SLOT_FACTORY_FOR_COMPOSITION,
        &ORIG_FACTORY_FOR_COMPOSITION,
    );
}

fn restore_slot(
    vtable: &AtomicPtr<*mut c_void>,
    index: usize,
    orig: &AtomicPtr<c_void>,
) {
    let vt = vtable.load(Ordering::Acquire);
    if vt.is_null() {
        return;
    }
    let o = orig.load(Ordering::Acquire);
    if !o.is_null() {
        unsafe { restore_vtable(vt, index, o) };
    }
    vtable.store(std::ptr::null_mut(), Ordering::Release);
}

// ---------------------------------------------------------------------------
// Hook bodies (hot path — lock-free, allocation-free, panic-free)
// ---------------------------------------------------------------------------

/// One-time `IDXGISwapChain2::SetMaximumFrameLatency(1)` on the active swap
/// chain: caps DXGI's pre-rendered frame queue at 1 so the JIT-paced schedule
/// actually reaches the display on time. Called ONLY while pacing is active —
/// with the limiter off iFrame is a pure pass-through and must not alter the
/// game's queue depth.
///
/// Uses the typed `IDXGISwapChain2` wrapper (correct IID + correct vtable slot
/// straight from the windows crate) instead of a hand-typed GUID: the previous
/// version carried a wrong IID, so its QueryInterface silently failed and the
/// call never happened. One attempt per distinct swap chain pointer — swap
/// chains get recreated, and unsupported (pre-DXGI-1.3) objects must not be
/// re-QI'd on every present.
fn ensure_maximum_frame_latency(this: *mut c_void) {
    static LAST_SC: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static STATE: AtomicU32 = AtomicU32::new(0); // 0 = untried, 1 = applied, 2 = unsupported
    if LAST_SC.load(Ordering::Relaxed) == this && STATE.load(Ordering::Relaxed) != 0 {
        return;
    }
    LAST_SC.store(this, Ordering::Relaxed);
    // Borrow the COM object without owning it: ManuallyDrop skips the Release
    // (we hold no reference of our own). `cast` runs a real QueryInterface
    // with the crate's own IID; the returned wrapper Releases on drop.
    let sc = std::mem::ManuallyDrop::new(unsafe {
        std::mem::transmute::<*mut c_void, IDXGISwapChain>(this)
    });
    match sc.cast::<IDXGISwapChain2>() {
        Ok(sc2) => {
            unsafe {
                let _ = sc2.SetMaximumFrameLatency(1);
            }
            STATE.store(1, Ordering::Relaxed);
            crate::log_line("SetMaximumFrameLatency(1) applied via IDXGISwapChain2");
        }
        Err(_) => STATE.store(2, Ordering::Relaxed),
    }
}

static LAST_PRESENT_QPC: AtomicU64 = AtomicU64::new(0);
static SMOOTH_FT_US: AtomicU64 = AtomicU64::new(16666);

#[inline]
unsafe fn pre_present_hud_and_reflex(this: *mut c_void, cfg: &iframe_common::config::RuntimeConfig) {
    let reflex = crate::reflex::get();
    if reflex.is_available() {
        let sc = std::mem::ManuallyDrop::new(unsafe {
            std::mem::transmute::<*mut c_void, IDXGISwapChain>(this)
        });
        if let Ok(dev) = sc.GetDevice::<ID3D11Device>() {
            let dev_raw = dev.as_raw();
            reflex.configure(dev_raw, cfg.reflex_mode, cfg.target_fps);
            if cfg.reflex_mode != iframe_common::config::ReflexMode::Off {
                reflex.sleep(dev_raw);
            }
            reflex.set_marker(dev_raw, crate::reflex::LatencyMarkerType::PresentStart, 0);
        }
    }

    if cfg.overlay_enabled {
        let now = crate::timing::qpc_now();
        let prev = LAST_PRESENT_QPC.swap(now as u64, Ordering::Relaxed);
        let dt_us = if prev > 0 && now as u64 > prev {
            ((now - prev as i64) as f64 * 1_000_000.0) / crate::timing::qpc_frequency() as f64
        } else {
            16666.0
        };
        let prev_ema = SMOOTH_FT_US.load(Ordering::Relaxed) as f64;
        let new_ema = prev_ema * 0.9 + dt_us * 0.1;
        SMOOTH_FT_US.store(new_ema as u64, Ordering::Relaxed);
        let ft_ms = new_ema / 1000.0;
        let fps = if ft_ms > 0.1 { 1000.0 / ft_ms } else { 0.0 };

        let mode_str = match cfg.mode {
            PacerMode::FixedVsync => "FixedVsync",
            PacerMode::Vrr => "VRR",
            PacerMode::Bypass => "Bypass",
        };
        let reflex_status = if reflex.is_available() {
            match cfg.reflex_mode {
                iframe_common::config::ReflexMode::Off => "Off",
                iframe_common::config::ReflexMode::On => "On",
                iframe_common::config::ReflexMode::Boost => "Boost",
            }
        } else {
            "N/A"
        };
        crate::overlay::d3d11::render(this, fps, ft_ms, mode_str, reflex_status);
    }
}

#[inline]
unsafe fn post_present_reflex(this: *mut c_void) {
    let reflex = crate::reflex::get();
    if reflex.is_available() {
        let sc = std::mem::ManuallyDrop::new(unsafe {
            std::mem::transmute::<*mut c_void, IDXGISwapChain>(this)
        });
        if let Ok(dev) = sc.GetDevice::<ID3D11Device>() {
            reflex.set_marker(dev.as_raw(), crate::reflex::LatencyMarkerType::PresentEnd, 0);
        }
    }
}

/// JIT-paced Present: the real Present executes IMMEDIATELY (the frame is
/// never held); the sleep happens after it, delaying only the START of the
/// next frame so it completes just-in-time for its target vblank.
unsafe extern "system" fn hooked_present(this: *mut c_void, sync_interval: u32, flags: u32) -> i32 {
    let t_start = crate::timing::qpc_now();
    let cfg = telemetry::config();
    let active = is_active_swapchain(this);
    let pacing = active && cfg.enabled && cfg.mode != PacerMode::Bypass;
    if pacing {
        ensure_maximum_frame_latency(this);
    }
    // К1: while pacing, OUR timer owns the timing — the game's VSync wait
    // inside Present would double-wait. Tearing-capable swap chains get
    // SyncInterval=0 + ALLOW_TEARING (exact delivery); windowed flip gets
    // SyncInterval=0 (DWM still composites at vblank); exclusive fullscreen
    // keeps the game's own VSync.
    let (sync, present_flags) = if pacing {
        unsafe { override_sync(sync_interval, flags, cfg.vsync_override) }
    } else {
        (sync_interval, flags)
    };

    pre_present_hud_and_reflex(this, &cfg);

    let orig = ORIG_PRESENT.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32 // E_FAIL — trampoline missing, should not happen
    } else {
        (std::mem::transmute::<*mut c_void, PresentFn>(orig))(this, sync, present_flags)
    };

    post_present_reflex(this);

    let t_end = crate::timing::qpc_now();
    if pacing {
        let hint = crate::vblank::hint();
        if let Some((release, decision)) = crate::engine::pace(t_start, t_end, &cfg, hint) {
            telemetry::record_paced(t_start, t_end, release, &decision, sync == 0);
            return hr;
        }
    }
    if active {
        telemetry::record_present(t_start, t_end);
    }
    hr
}

unsafe extern "system" fn hooked_present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    present_parameters: *const DXGI_PRESENT_PARAMETERS,
) -> i32 {
    let t_start = crate::timing::qpc_now();
    let cfg = telemetry::config();
    let active = is_active_swapchain(this);
    let pacing = active && cfg.enabled && cfg.mode != PacerMode::Bypass;
    if pacing {
        ensure_maximum_frame_latency(this);
    }
    let (sync, present_flags) = if pacing {
        unsafe { override_sync(sync_interval, flags, cfg.vsync_override) }
    } else {
        (sync_interval, flags)
    };

    pre_present_hud_and_reflex(this, &cfg);

    let orig = ORIG_PRESENT1.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, Present1Fn>(orig))(
            this,
            sync,
            present_flags,
            present_parameters,
        )
    };

    post_present_reflex(this);

    let t_end = crate::timing::qpc_now();
    if pacing {
        let hint = crate::vblank::hint();
        if let Some((release, decision)) = crate::engine::pace(t_start, t_end, &cfg, hint) {
            telemetry::record_paced(t_start, t_end, release, &decision, sync == 0);
            return hr;
        }
    }
    if active {
        telemetry::record_present(t_start, t_end);
    }
    hr
}

/// Set cached capabilities directly for unit testing.
pub fn set_caps_for_test(windowed: bool, tearing: bool, waitable: bool) {
    CAPS_WINDOWED.store(windowed, Ordering::Release);
    CAPS_TEARING.store(tearing, Ordering::Release);
    CAPS_WAITABLE.store(waitable, Ordering::Release);
    CAPS_VALID.store(true, Ordering::Release);
}

/// К1 sync/flags override for paced presents. `enabled` mirrors the app's
/// "override VSync" checkbox — off means the game's own present args pass
/// through untouched.
pub unsafe fn override_sync(sync_interval: u32, flags: u32, enabled: bool) -> (u32, u32) {
    if !enabled {
        return (sync_interval, flags);
    }
    ensure_caps();
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        crate::log_line(&format!(
            "override_sync caps: windowed={} tearing={} waitable={}",
            CAPS_WINDOWED.load(Ordering::Relaxed),
            CAPS_TEARING.load(Ordering::Relaxed),
            CAPS_WAITABLE.load(Ordering::Relaxed),
        ));
    }
    if CAPS_TEARING.load(Ordering::Relaxed) {
        (0, flags | DXGI_PRESENT_ALLOW_TEARING.0)
    } else if CAPS_WINDOWED.load(Ordering::Relaxed) {
        (0, flags)
    } else {
        // Exclusive fullscreen without tearing: leave the game's VSync alone.
        (sync_interval, flags)
    }
}

/// ResizeBuffers invalidates the cached caps (they may be renegotiated).
unsafe extern "system" fn hooked_resize_buffers(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: u32,
    flags: u32,
) -> i32 {
    // Release any cached D3D11 overlay RTV so ResizeBuffers does not fail with DXGI_ERROR_INVALID_CALL
    crate::overlay::d3d11::on_resize();

    let orig = ORIG_RESIZE_BUFFERS.load(Ordering::Acquire);
    // К5: when we forced FRAME_LATENCY_WAITABLE_OBJECT at creation, the game's
    // own ResizeBuffers call without the flag would fail — add it back.
    let flags = if telemetry::config().force_waitable
        && flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 == 0
    {
        flags | DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32
    } else {
        flags
    };
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, ResizeBuffersFn>(orig))(
            this, buffer_count, width, height, format, flags,
        )
    };

    if hr >= 0 {
        CAPS_VALID.store(false, Ordering::Release);
    }
    hr
}

// ---------------------------------------------------------------------------
// Swap-chain creation hooks (factory family)
// ---------------------------------------------------------------------------

unsafe extern "system" fn hooked_create_swap_chain(
    this: *mut c_void,
    device: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC,
    out: *mut *mut c_void,
) -> i32 {
    let orig = ORIG_FACTORY_CREATE.load(Ordering::Acquire);
    let forced = force_waitable_desc(desc);
    let desc_ptr = forced.as_ref().map(|d| d as *const _).unwrap_or(desc);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, CreateSwapChainFn>(orig))(
            this, device, desc_ptr, out,
        )
    };
    if hr >= 0 && !out.is_null() && !(*out).is_null() && !desc.is_null() {
        let d = &*desc_ptr;
        on_swapchain_created(
            *out,
            d.Windowed.as_bool(),
            d.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0,
            d.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0,
            d.BufferCount,
        );
    }
    hr
}

unsafe extern "system" fn hooked_create_for_hwnd(
    this: *mut c_void,
    device: *mut c_void,
    hwnd: HWND,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    fullscreen_desc: *const windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    let orig = ORIG_FACTORY_FOR_HWND.load(Ordering::Acquire);
    let forced = force_waitable_desc1(desc);
    let desc_ptr = forced.as_ref().map(|d| d as *const _).unwrap_or(desc);
    let in_flags = if !desc.is_null() { (*desc).Flags } else { 0 };
    let out_flags = if !desc_ptr.is_null() { (*desc_ptr).Flags } else { 0 };
    crate::log_line(&format!(
        "hooked_create_for_hwnd: orig={:?} in_flags={:#x} out_flags={:#x} forced={}",
        orig, in_flags, out_flags, forced.is_some()
    ));
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, CreateSwapChainForHwndFn>(orig))(
            this, device, hwnd, desc_ptr, fullscreen_desc, restrict_to_output, out,
        )
    };
    crate::log_line(&format!("hooked_create_for_hwnd returned hr={:#x}", hr as u32));
    if hr >= 0 && !out.is_null() && !(*out).is_null() && !desc.is_null() {
        let d = &*desc_ptr;
        // fullscreen_desc == NULL → windowed; otherwise honour its Windowed.
        let windowed = fullscreen_desc.is_null() || (*fullscreen_desc).Windowed.as_bool();
        on_swapchain_created(
            *out,
            windowed,
            d.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0,
            d.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0,
            d.BufferCount,
        );
    }
    hr
}

unsafe extern "system" fn hooked_create_for_core_window(
    this: *mut c_void,
    device: *mut c_void,
    window: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    let orig = ORIG_FACTORY_FOR_CORE_WINDOW.load(Ordering::Acquire);
    let forced = force_waitable_desc1(desc);
    let desc_ptr = forced.as_ref().map(|d| d as *const _).unwrap_or(desc);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, CreateSwapChainForCoreWindowFn>(orig))(
            this, device, window, desc_ptr, restrict_to_output, out,
        )
    };
    if hr >= 0 && !out.is_null() && !(*out).is_null() && !desc.is_null() {
        let d = &*desc_ptr;
        on_swapchain_created(
            *out,
            true, // CoreWindow swap chains are always composited (windowed)
            d.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0,
            d.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0,
            d.BufferCount,
        );
    }
    hr
}

unsafe extern "system" fn hooked_create_for_composition(
    this: *mut c_void,
    device: *mut c_void,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    restrict_to_output: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    let orig = ORIG_FACTORY_FOR_COMPOSITION.load(Ordering::Acquire);
    let forced = force_waitable_desc1(desc);
    let desc_ptr = forced.as_ref().map(|d| d as *const _).unwrap_or(desc);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        (std::mem::transmute::<*mut c_void, CreateSwapChainForCompositionFn>(orig))(
            this, device, desc_ptr, restrict_to_output, out,
        )
    };
    if hr >= 0 && !out.is_null() && !(*out).is_null() && !desc.is_null() {
        let d = &*desc_ptr;
        on_swapchain_created(
            *out,
            true, // composition swap chains are always windowed
            d.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0,
            d.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0,
            d.BufferCount,
        );
    }
    hr
}

/// Per-game opt-in (`RuntimeConfig.force_waitable`): when the config asks for
/// it and the game did not set FRAME_LATENCY_WAITABLE_OBJECT itself, return a
/// DESC copy with the flag added (SpecialK-style). `None` = pass through.
pub unsafe fn force_waitable_desc(desc: *const DXGI_SWAP_CHAIN_DESC) -> Option<DXGI_SWAP_CHAIN_DESC> {
    if desc.is_null() || !telemetry::config().force_waitable {
        return None;
    }
    if (*desc).Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0 {
        return None;
    }
    let mut d = *desc;
    d.Flags |= DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
    Some(d)
}

/// Same for the DESC1 creation family.
pub unsafe fn force_waitable_desc1(
    desc: *const DXGI_SWAP_CHAIN_DESC1,
) -> Option<DXGI_SWAP_CHAIN_DESC1> {
    if desc.is_null() || !telemetry::config().force_waitable {
        return None;
    }
    if (*desc).Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0 {
        return None;
    }
    let mut d = *desc;
    d.Flags |= DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
    Some(d)
}

/// A new swap chain appeared: it becomes the active one and its capabilities
/// are cached straight from the creation desc (zero COM calls).
unsafe fn on_swapchain_created(
    swapchain: *mut c_void,
    windowed: bool,
    tearing: bool,
    waitable: bool,
    buffers: u32,
) {
    ACTIVE_SWAPCHAIN.store(swapchain, Ordering::Release);
    CAPS_WINDOWED.store(windowed, Ordering::Relaxed);
    CAPS_TEARING.store(tearing, Ordering::Relaxed);
    CAPS_WAITABLE.store(waitable, Ordering::Relaxed);
    CAPS_VALID.store(true, Ordering::Release);
    crate::log_line(&format!(
        "swapchain created: windowed={windowed} tearing={tearing} waitable={waitable} buffers={buffers}"
    ));
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

/// Refresh the caps cache when invalid (lazily, once per invalidation).
fn ensure_caps() {
    if CAPS_VALID.load(Ordering::Relaxed) {
        return;
    }
    let active = ACTIVE_SWAPCHAIN.load(Ordering::Relaxed);
    if active.is_null() {
        return;
    }
    let (windowed, tearing, waitable) = unsafe { query_swapchain_caps(active) };
    CAPS_WINDOWED.store(windowed, Ordering::Relaxed);
    CAPS_TEARING.store(tearing, Ordering::Relaxed);
    CAPS_WAITABLE.store(waitable, Ordering::Relaxed);
    CAPS_VALID.store(true, Ordering::Release);
}

/// `IDXGISwapChain::GetDesc` called MANUALLY through the object's vtable
/// (slot 12). The legacy desc carries the `Flags` field, so ONE call yields
/// windowed-ness, ALLOW_TEARING and the waitable-object flag — no GetDesc1
/// (slot 17) needed: the object's stored vtable pointer may be the BASE
/// SwapChain vtable, where slot 17 is past the end (returns garbage).
///
/// IMPORTANT: do NOT construct a `&IDXGISwapChain` wrapper from the raw `this`
/// pointer with `&*(this as *const IDXGISwapChain)` — the wrapper struct
/// *contains* the object pointer, so that cast makes its inner field read the
/// object's first 8 bytes (= the vtable pointer) as if they were the object
/// pointer. Every COM call through such a wrapper dereferences garbage →
/// access violation (the exact crash this replaced).
unsafe fn query_swapchain_caps(this: *mut c_void) -> (bool, bool, bool) {
    // The COM object's first field is the vtable pointer.
    let vtbl = *(this as *const *mut *mut c_void);
    if vtbl.is_null() {
        crate::log_line("query_swapchain_caps: null vtable");
        return (true, false, false);
    }
    let get_desc_raw = *vtbl.add(12);
    if get_desc_raw.is_null() {
        return (true, false, false);
    }
    type GetDescFn = unsafe extern "system" fn(*mut c_void, *mut DXGI_SWAP_CHAIN_DESC) -> i32;
    let mut desc: DXGI_SWAP_CHAIN_DESC = core::mem::zeroed();
    let hr = std::mem::transmute::<*mut c_void, GetDescFn>(get_desc_raw)(this, &mut desc);
    if hr < 0 {
        crate::log_line(&format!("GetDesc failed: {hr:#x}"));
        return (true, false, false);
    }
    let tearing = desc.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0;
    let waitable = desc.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0;
    crate::log_line(&format!(
        "GetDesc: flags={:#x} windowed={} tearing={tearing} waitable={waitable} buffers={}",
        desc.Flags,
        desc.Windowed.as_bool(),
        desc.BufferCount
    ));
    (desc.Windowed.as_bool(), tearing, waitable)
}

// ---------------------------------------------------------------------------
// Vtable patching
// ---------------------------------------------------------------------------

/// Overwrite `vtable[index]` with `hook`; returns the original pointer.
pub unsafe fn patch_vtable_slot(
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

pub unsafe fn restore_vtable(vtable: *mut *mut c_void, index: usize, orig: *mut c_void) {
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

unsafe fn create_dummy_device_and_swapchain(
    hwnd: HWND,
) -> Result<(ID3D11Device, IDXGISwapChain), String> {
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
    let swapchain = swapchain.ok_or_else(|| "no swap chain returned".to_string())?;
    let device = device.ok_or_else(|| "no device returned".to_string())?;
    Ok((device, swapchain))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-counted slot constants above MUST match the real COM ABI.
    /// Derives the truth from live COM objects: the pointer stored in the
    /// object's vtable at `base + index * 8` must equal the typed vtable field
    /// generated by the windows crate from the SDK metadata. This test exists
    /// because Present1 was silently patched at GetCoreWindow's slot (21
    /// instead of 22) and the factory family was off by one.
    #[test]
    fn vtable_slot_indices_match_real_com_objects() {
        let hwnd = create_hidden_window().expect("dummy window");
        let (device, swapchain) =
            unsafe { create_dummy_device_and_swapchain(hwnd) }.expect("dummy swapchain");

        let raw = unsafe { *(swapchain.as_raw() as *mut *mut *mut c_void) };
        let sc2: IDXGISwapChain2 = swapchain.cast().expect("SwapChain2");
        let vt = windows::core::Interface::vtable(&sc2);
        let slot = |i: usize| unsafe { core::ptr::read(raw.add(i)) } as usize;

        assert_eq!(
            slot(SLOT_PRESENT),
            vt.base__.base__.Present as usize,
            "SLOT_PRESENT must be the real Present"
        );
        assert_eq!(
            slot(SLOT_RESIZE_BUFFERS),
            vt.base__.base__.ResizeBuffers as usize,
            "SLOT_RESIZE_BUFFERS must be the real ResizeBuffers"
        );
        assert_eq!(
            slot(SLOT_PRESENT1),
            vt.base__.Present1 as usize,
            "SLOT_PRESENT1 must be the real Present1 (22, not GetCoreWindow@21)"
        );
        assert_eq!(
            slot(21),
            vt.base__.GetCoreWindow as usize,
            "slot 21 is GetCoreWindow — regression guard for the old off-by-one"
        );

        let dxgi_device: IDXGIDevice = device.cast().expect("IDXGIDevice");
        let adapter = unsafe { dxgi_device.GetAdapter() }.expect("adapter");
        let factory: windows::Win32::Graphics::Dxgi::IDXGIFactory2 =
            unsafe { adapter.GetParent() }.expect("factory");
        let fraw = unsafe { *(factory.as_raw() as *mut *mut *mut c_void) };
        let fvt = windows::core::Interface::vtable(&factory);
        let fslot = |i: usize| unsafe { core::ptr::read(fraw.add(i)) } as usize;

        assert_eq!(
            fslot(SLOT_FACTORY_CREATE),
            fvt.base__.base__.CreateSwapChain as usize,
            "SLOT_FACTORY_CREATE must be the real CreateSwapChain"
        );
        assert_eq!(
            fslot(SLOT_FACTORY_FOR_HWND),
            fvt.CreateSwapChainForHwnd as usize,
            "SLOT_FACTORY_FOR_HWND must be the real CreateSwapChainForHwnd"
        );
        assert_eq!(
            fslot(SLOT_FACTORY_FOR_CORE_WINDOW),
            fvt.CreateSwapChainForCoreWindow as usize,
            "SLOT_FACTORY_FOR_CORE_WINDOW must be the real CreateSwapChainForCoreWindow"
        );
        assert_eq!(
            fslot(SLOT_FACTORY_FOR_COMPOSITION),
            fvt.CreateSwapChainForComposition as usize,
            "SLOT_FACTORY_FOR_COMPOSITION must be the real CreateSwapChainForComposition"
        );

        drop(sc2);
        drop(factory);
        drop(adapter);
        drop(dxgi_device);
        drop(swapchain);
        drop(device);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

