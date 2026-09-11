use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, ID3D11DepthStencilView,
    ID3D11Device, ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, IDXGISwapChain1, DXGI_PRESENT_PARAMETERS, DXGI_SWAP_CHAIN_DESC,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::Memory::{VirtualProtect, PAGE_PROTECTION_FLAGS, PAGE_READWRITE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, RegisterClassW, CS_HREDRAW, CS_VREDRAW,
    WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use aif_common::ipc::SharedMemoryChannel;
use aif_common::WarpConfig;

use crate::depth::GLOBAL_DEPTH_TRACKER;
use crate::raw_input::start_raw_input_listener;
use crate::warp_engine::GLOBAL_WARP_CONTEXT;

const SLOT_PRESENT: usize = 8;
const SLOT_PRESENT1: usize = 22;
const SLOT_CLEAR_DEPTH_STENCIL: usize = 53;

type PresentFn = unsafe extern "system" fn(this: *mut c_void, sync_interval: u32, flags: u32) -> i32;
type Present1Fn = unsafe extern "system" fn(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    params: *const DXGI_PRESENT_PARAMETERS,
) -> i32;
type ClearDepthStencilViewFn = unsafe extern "system" fn(
    this: *mut c_void,
    p_dsv: *mut c_void,
    clear_flags: u32,
    depth: f32,
    stencil: u8,
);

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_PRESENT1: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CLEAR_DEPTH_STENCIL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);

static GLOBAL_CONFIG: std::sync::RwLock<WarpConfig> = std::sync::RwLock::new(WarpConfig {
    enabled: true,
    yaw_sensitivity: 0.0015,
    pitch_sensitivity: 0.0015,
    fov_degrees: 90.0,
    depth_near: 0.1,
    depth_far: 1000.0,
    is_reverse_z: false,
    hud_mask_enabled: false,
    hud_depth_threshold: 0.005,
    inpainting_strength: 0.8,
    fps_multiplier: 1,
    continuous_test_wave: false,
    test_pulse: 0,
    debug_depth: false,
});

pub fn update_config(cfg: WarpConfig) {
    if let Ok(mut w) = GLOBAL_CONFIG.write() {
        *w = cfg;
    }
}

pub fn current_config() -> WarpConfig {
    GLOBAL_CONFIG.read().map(|r| *r).unwrap_or_default()
}

static LAST_FRAME_TIME: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
static SMOOTH_GAME_FPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn update_fps_telemetry(h: &aif_common::ipc::AifSharedHeader, multiplier: u32) -> std::time::Duration {
    let now = std::time::Instant::now();
    let mut guard = LAST_FRAME_TIME.lock().unwrap();
    let delta = if let Some(last) = *guard {
        now.duration_since(last)
    } else {
        std::time::Duration::from_millis(33)
    };
    *guard = Some(now);

    let dt = delta.as_secs_f32().max(0.001);
    let instant_fps = 1.0 / dt;
    let old_fps = f32::from_bits(SMOOTH_GAME_FPS.load(Ordering::Relaxed));
    let smooth_fps = if old_fps <= 0.0 { instant_fps } else { old_fps * 0.9 + instant_fps * 0.1 };
    SMOOTH_GAME_FPS.store(smooth_fps.to_bits(), Ordering::Relaxed);

    let mult = multiplier.clamp(1, 3) as f32;
    let output_fps = smooth_fps * mult;
    h.game_fps_bits.store(smooth_fps.to_bits(), Ordering::Release);
    h.warped_fps_bits.store(output_fps.to_bits(), Ordering::Release);

    delta
}

unsafe extern "system" fn hooked_clear_depth_stencil_view(
    this: *mut c_void,
    p_dsv: *mut c_void,
    clear_flags: u32,
    depth: f32,
    stencil: u8,
) {
    if !p_dsv.is_null() {
        let dsv = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, ID3D11DepthStencilView>(p_dsv));
        if let Ok(res) = dsv.GetResource() {
            if let Ok(mut tracker) = GLOBAL_DEPTH_TRACKER.lock() {
                tracker.register_depth_resource(res);
            }
        }
    }

    let orig = ORIG_CLEAR_DEPTH_STENCIL.load(Ordering::Acquire);
    if !orig.is_null() {
        (std::mem::transmute::<*mut c_void, ClearDepthStencilViewFn>(orig))(
            this, p_dsv, clear_flags, depth, stencil,
        );
    }
}

unsafe extern "system" fn hooked_present(this: *mut c_void, sync_interval: u32, flags: u32) -> i32 {
    let (cfg, frame_delta) = if let Ok(shm) = SharedMemoryChannel::open_or_create() {
        let h = shm.header();
        h.hook_state.store(1, Ordering::Release);
        let has_depth = GLOBAL_DEPTH_TRACKER.lock().map(|t| t.has_active_depth()).unwrap_or(false);
        h.depth_found.store(if has_depth { 1 } else { 0 }, Ordering::Release);
        let cfg = h.read_config();
        let delta = update_fps_telemetry(h, cfg.fps_multiplier);
        h.frames_warped.fetch_add(cfg.fps_multiplier.max(1) as u64, Ordering::Relaxed);
        (cfg, delta)
    } else {
        (current_config(), std::time::Duration::from_millis(33))
    };

    if cfg.enabled {
        let sc = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, IDXGISwapChain>(this));
        if let Ok(mut warp) = GLOBAL_WARP_CONTEXT.try_lock() {
            let _ = warp.process_frame(&sc, &cfg, false);
        }
    }

    let orig = ORIG_PRESENT.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0
    } else {
        (std::mem::transmute::<*mut c_void, PresentFn>(orig))(this, sync_interval, flags)
    };

    // Predictive Frame Extrapolation (2x / 3x intermediate presentation)
    if cfg.enabled && cfg.fps_multiplier > 1 && !orig.is_null() {
        let sc = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, IDXGISwapChain>(this));
        let num_extra = cfg.fps_multiplier.saturating_sub(1);
        let sleep_slice = frame_delta / cfg.fps_multiplier;
        for _ in 0..num_extra {
            std::thread::sleep(sleep_slice.clamp(std::time::Duration::from_millis(2), std::time::Duration::from_millis(33)));
            if let Ok(mut warp) = GLOBAL_WARP_CONTEXT.try_lock() {
                let _ = warp.process_frame(&sc, &cfg, true);
            }
            (std::mem::transmute::<*mut c_void, PresentFn>(orig))(this, 0, flags);
        }
    }

    hr
}

unsafe extern "system" fn hooked_present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    params: *const DXGI_PRESENT_PARAMETERS,
) -> i32 {
    let (cfg, frame_delta) = if let Ok(shm) = SharedMemoryChannel::open_or_create() {
        let h = shm.header();
        h.hook_state.store(1, Ordering::Release);
        let has_depth = GLOBAL_DEPTH_TRACKER.lock().map(|t| t.has_active_depth()).unwrap_or(false);
        h.depth_found.store(if has_depth { 1 } else { 0 }, Ordering::Release);
        let cfg = h.read_config();
        let delta = update_fps_telemetry(h, cfg.fps_multiplier);
        h.frames_warped.fetch_add(cfg.fps_multiplier.max(1) as u64, Ordering::Relaxed);
        (cfg, delta)
    } else {
        (current_config(), std::time::Duration::from_millis(33))
    };

    if cfg.enabled {
        let sc = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, IDXGISwapChain>(this));
        if let Ok(mut warp) = GLOBAL_WARP_CONTEXT.try_lock() {
            let _ = warp.process_frame(&sc, &cfg, false);
        }
    }

    let orig = ORIG_PRESENT1.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0
    } else {
        (std::mem::transmute::<*mut c_void, Present1Fn>(orig))(this, sync_interval, flags, params)
    };

    // Predictive Frame Extrapolation (2x / 3x intermediate presentation)
    if cfg.enabled && cfg.fps_multiplier > 1 && !orig.is_null() {
        let sc = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, IDXGISwapChain>(this));
        let num_extra = cfg.fps_multiplier.saturating_sub(1);
        let sleep_slice = frame_delta / cfg.fps_multiplier;
        for _ in 0..num_extra {
            std::thread::sleep(sleep_slice.clamp(std::time::Duration::from_millis(2), std::time::Duration::from_millis(33)));
            if let Ok(mut warp) = GLOBAL_WARP_CONTEXT.try_lock() {
                let _ = warp.process_frame(&sc, &cfg, true);
            }
            (std::mem::transmute::<*mut c_void, Present1Fn>(orig))(this, 0, flags, params);
        }
    }

    hr
}

unsafe fn patch_vtable_slot(
    vtable: *mut *mut c_void,
    index: usize,
    hook: *mut c_void,
) -> Result<*mut c_void, String> {
    let slot = vtable.add(index);
    let page = (slot as usize) & !0xFFF;
    let mut old = PAGE_PROTECTION_FLAGS(0);
    VirtualProtect(page as *const c_void, 4096, PAGE_READWRITE, &mut old)
        .map_err(|e| format!("VirtualProtect slot {index}: {e}"))?;
    let orig = std::ptr::read(slot);
    std::ptr::write(slot, hook);
    let mut tmp = PAGE_PROTECTION_FLAGS(0);
    let _ = VirtualProtect(page as *const c_void, 4096, old, &mut tmp);
    Ok(orig)
}

pub unsafe fn install_hooks() -> Result<(), String> {
    if HOOKS_INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    // Start 1000 Hz Raw Input listener
    start_raw_input_listener();

    // Create dummy window and swap chain to obtain live vtable address
    let hwnd = create_hidden_window().map_err(|e| format!("Window creation failed: {e}"))?;
    let (_device, swapchain, context) =
        create_dummy_device_and_swapchain(hwnd).map_err(|e| format!("D3D11 dummy failed: {e}"))?;

    // 1. Patch SwapChain Present (slot 8)
    let raw_sc = *(swapchain.as_raw() as *mut *mut *mut c_void);
    let orig_p = patch_vtable_slot(raw_sc, SLOT_PRESENT, hooked_present as *mut c_void)?;
    ORIG_PRESENT.store(orig_p, Ordering::Release);

    // 2. Patch Present1 (slot 22) if available
    if let Ok(sc1) = swapchain.cast::<IDXGISwapChain1>() {
        let raw1 = *(sc1.as_raw() as *mut *mut *mut c_void);
        if let Ok(orig_p1) = patch_vtable_slot(raw1, SLOT_PRESENT1, hooked_present1 as *mut c_void) {
            ORIG_PRESENT1.store(orig_p1, Ordering::Release);
        }
    }

    // 3. Patch Context ClearDepthStencilView (slot 53)
    let raw_ctx = *(context.as_raw() as *mut *mut *mut c_void);
    if let Ok(orig_clear) = patch_vtable_slot(raw_ctx, SLOT_CLEAR_DEPTH_STENCIL, hooked_clear_depth_stencil_view as *mut c_void) {
        ORIG_CLEAR_DEPTH_STENCIL.store(orig_clear, Ordering::Release);
    }

    let _ = DestroyWindow(hwnd);
    Ok(())
}

unsafe extern "system" fn dummy_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn create_hidden_window() -> Result<HWND, windows::core::Error> {
    let class_name = w!("AllInFrameDummyWindowClass");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(dummy_wnd_proc),
        lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
        ..Default::default()
    };

    unsafe {
        let _ = RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR::from_raw(class_name.as_ptr()),
            w!("AllInFrameDummyWindow"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            64,
            64,
            None,
            None,
            None,
            None,
        )
    }
}

fn create_dummy_device_and_swapchain(
    hwnd: HWND,
) -> Result<(ID3D11Device, IDXGISwapChain, ID3D11DeviceContext), windows::core::Error> {
    let sc_desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 64,
            Height: 64,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        OutputWindow: hwnd,
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..Default::default()
    };

    let mut device = None;
    let mut swapchain = None;
    let mut context = None;
    let mut feature_level = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&sc_desc),
            Some(&mut swapchain),
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )?;
    }

    Ok((device.unwrap(), swapchain.unwrap(), context.unwrap()))
}
