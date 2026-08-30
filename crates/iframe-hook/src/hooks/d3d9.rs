//! D3D9 hooks: `IDirect3DDevice9::Present` (vtable slot 17).
//!
//! d3d9.dll gives EVERY device object its own heap copy of the vtable (an
//! anti-hook measure — verified empirically: two devices in one process have
//! different heap vtable pointers). Patching one device's copy does not
//! affect others, and IAT-hooking `Direct3DCreate9` proved unreliable
//! (the patched slot was silently reverted before the game's call).
//!
//! The working approach is TEMPLATE patching: a device's vtable copy is made
//! from a template inside the d3d9.dll image. We create a dummy device, read
//! its copy's first 16 pointers as a signature, scan the d3d9.dll image for
//! that signature (the template), and patch the TEMPLATE's Present slot.
//! Every device created afterwards receives a hooked copy. Verified by a
//! self-test: a second dummy device must call our hook.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D9::{
    Direct3DCreate9, IDirect3DDevice9, D3DDEVTYPE_HAL, D3DFORMAT, D3DPRESENT_PARAMETERS,
    D3DSWAPEFFECT_DISCARD, D3D_SDK_VERSION,
};
use windows::Win32::Graphics::Gdi::RGNDATA;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::IMAGE_DOS_HEADER;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, SW_HIDE, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use iframe_common::pacer::PacerMode;

use crate::telemetry;

/// IDirect3DDevice9::Present (IUnknown 0-2 + 13 device methods; Reset is 16).
const SLOT_DEVICE_PRESENT: usize = 17;
/// How many leading vtable slots identify the template uniquely.
const SIGNATURE_SLOTS: usize = 16;

type PresentFn = unsafe extern "system" fn(
    this: *mut c_void,
    source: *const RECT,
    dest: *const RECT,
    dest_window_override: HWND,
    dirty_region: *const RGNDATA,
) -> i32;

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE: AtomicPtr<*mut c_void> = AtomicPtr::new(std::ptr::null_mut());
static SELF_TEST_FIRED: AtomicBool = AtomicBool::new(false);
/// Send-able vtable pointer wrapper (raw pointers are !Send by default).
#[derive(Clone)]
struct SendVtable(*mut *mut c_void);
unsafe impl Send for SendVtable {}
/// Hand-off to the re-patch daemon (statics avoid closure-capture Send issues).
static DAEMON_TEMPLATES: std::sync::Mutex<Vec<SendVtable>> =
    std::sync::Mutex::new(Vec::new());
static DAEMON_HOOK: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Install D3D9 hooks by patching the vtable TEMPLATE inside d3d9.dll.
/// Fails softly: `Err` means "no D3D9 here", which is normal for DXGI-only
/// games.
pub unsafe fn install() -> Result<(), String> {
    if !ORIG_PRESENT.load(Ordering::Acquire).is_null() {
        return Ok(());
    }

    let d3d9 = unsafe {
        GetModuleHandleW(PCWSTR::from_raw(w!("d3d9.dll").as_ptr()))
            .map_err(|_| "d3d9.dll not loaded".to_string())?
    };

    // 1. A dummy device — its vtable is a heap copy of the template.
    let hwnd = create_hidden_window().map_err(|e| format!("d3d9 dummy window: {e}"))?;
    let device = unsafe { create_dummy_device(hwnd).map_err(|e| e.to_string())? };
    let copy = *(device.as_raw() as *mut *mut *mut c_void);
    if copy.is_null() {
        return Err("null device vtable".into());
    }

    // 2. Locate ALL templates inside the d3d9.dll image by signature — the
    // module may hold several copies and device creations pick between them.
    let templates = find_vtable_templates(d3d9.0 as *const u8, copy);
    if templates.is_empty() {
        return Err("vtable template not found in the d3d9.dll image".into());
    }

    // 3. Patch EVERY template's Present slot (COW pages — this process only).
    let mut orig: *mut c_void = std::ptr::null_mut();
    for t in &templates {
        let o = crate::hooks::dxgi::patch_vtable_slot(
            *t,
            SLOT_DEVICE_PRESENT,
            hooked_present as *mut c_void,
        );
        if o.is_ok() && orig.is_null() {
            orig = o.unwrap();
        }
    }
    ORIG_PRESENT.store(orig, Ordering::Release);
    VTABLE.store(templates[0], Ordering::Release);
    crate::log_line(&format!(
        "d3d9: {} template(s) found and patched",
        templates.len()
    ));

    drop(device);
    let _ = DestroyWindow(hwnd);

    // 4. Self-test: a SECOND device must now carry the hook in its copy.
    let hwnd2 = create_hidden_window().map_err(|e| format!("d3d9 dummy window: {e}"))?;
    let device2 = unsafe { create_dummy_device(hwnd2).map_err(|e| e.to_string())? };
    let copy2 = *(device2.as_raw() as *mut *mut *mut c_void);
    let propagated =
        !copy2.is_null() && *copy2.add(SLOT_DEVICE_PRESENT) == hooked_present as *mut c_void;
    if propagated {
        // One Present through the hooked copy — must reach our hook.
        let _ = device2.Present(
            std::ptr::null(),
            std::ptr::null(),
            HWND::default(),
            std::ptr::null(),
        );
    }
    drop(device2);
    let _ = DestroyWindow(hwnd2);

    let fired = SELF_TEST_FIRED.load(Ordering::Relaxed);
    crate::log_line(&format!(
        "d3d9 hooks installed (templates={}, propagated={propagated}, fired={fired})",
        templates.len()
    ));
    if !propagated || !fired {
        return Err("template patch did not propagate to a new device".into());
    }

    // 5. d3d9.dll may hold several vtable templates and device creations pick
    // between them; a daemon keeps ALL of them patched.
    *DAEMON_TEMPLATES.lock().unwrap() = templates.iter().map(|t| SendVtable(*t)).collect();
    DAEMON_HOOK.store(hooked_present as *mut c_void, Ordering::Release);
    std::thread::spawn(|| {
        let hook_fn = DAEMON_HOOK.load(Ordering::Acquire);
        if hook_fn.is_null() {
            return;
        }
        for _ in 0..7200 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let templates = DAEMON_TEMPLATES.lock().unwrap().clone();
            for SendVtable(vtbl) in templates {
                unsafe {
                    let slot = vtbl.add(SLOT_DEVICE_PRESENT);
                    if std::ptr::read(slot) != hook_fn {
                        let _ = crate::hooks::dxgi::patch_vtable_slot(
                            vtbl,
                            SLOT_DEVICE_PRESENT,
                            hook_fn,
                        );
                        crate::log_line("d3d9: template re-patched (was reverted)");
                    }
                }
            }
        }
    });
    Ok(())
}

pub fn uninstall() {
    let vt = VTABLE.load(Ordering::Acquire);
    let orig = ORIG_PRESENT.load(Ordering::Acquire);
    if !vt.is_null() && !orig.is_null() {
        unsafe { crate::hooks::dxgi::restore_vtable(vt, SLOT_DEVICE_PRESENT, orig) };
    }
    VTABLE.store(std::ptr::null_mut(), Ordering::Release);
    ORIG_PRESENT.store(std::ptr::null_mut(), Ordering::Release);
}

// ---------------------------------------------------------------------------
// Hook body
// ---------------------------------------------------------------------------

/// JIT-paced D3D9 Present: the real Present executes immediately; the sleep
/// happens after it, delaying only the next frame's start. D3D9 has no
/// per-present sync interval, so no К1 override applies here.
unsafe extern "system" fn hooked_present(
    this: *mut c_void,
    source: *const RECT,
    dest: *const RECT,
    dest_window_override: HWND,
    dirty_region: *const RGNDATA,
) -> i32 {
    if !SELF_TEST_FIRED.swap(true, Ordering::Relaxed) {
        crate::log_line("d3d9 present hook FIRED");
    }
    let t_start = crate::timing::qpc_now();
    let cfg = telemetry::config();
    let pacing = cfg.enabled && cfg.mode != PacerMode::Bypass;
    let orig = ORIG_PRESENT.load(Ordering::Acquire);
    let hr = if orig.is_null() {
        0x8000_4005u32 as i32
    } else {
        unsafe {
            (std::mem::transmute::<*mut c_void, PresentFn>(orig))(
                this, source, dest, dest_window_override, dirty_region,
            )
        }
    };
    let t_end = crate::timing::qpc_now();
    if pacing {
        let hint = crate::vblank::hint();
        if let Some((release, decision)) = crate::engine::pace(t_start, t_end, &cfg, hint) {
            telemetry::record_paced(t_start, t_end, release, &decision, false);
            return hr;
        }
    }
    telemetry::record_present(t_start, t_end);
    hr
}

// ---------------------------------------------------------------------------
// Template search
// ---------------------------------------------------------------------------

/// Scan the d3d9.dll image for EVERY 16-pointer run matching the device's
/// vtable copy — each run is a template that device copies may be made from.
unsafe fn find_vtable_templates(base: *const u8, copy: *mut *mut c_void) -> Vec<*mut *mut c_void> {
    let mut found = Vec::new();
    if base.is_null() {
        return found;
    }
    let dos = &*(base as *const IMAGE_DOS_HEADER);
    if dos.e_magic != 0x5A4D {
        return found;
    }
    let opt = base.add(dos.e_lfanew as usize + 24);
    let size_of_image = match *(opt as *const u16) {
        0x20B | 0x10B => *(opt.add(56) as *const u32) as usize,
        _ => return found,
    };

    let slots = copy as *const usize;
    let first = *slots;
    let mut off = 0usize;
    while off + SIGNATURE_SLOTS * 8 <= size_of_image {
        let p = base.add(off) as *const usize;
        if *p == first {
            let mut ok = true;
            for i in 1..SIGNATURE_SLOTS {
                if *p.add(i) != *slots.add(i) {
                    ok = false;
                    break;
                }
            }
            if ok {
                found.push(p as *mut *mut c_void);
                off += SIGNATURE_SLOTS * 8; // skip past this run
                continue;
            }
        }
        off += 8;
    }
    found
}

// ---------------------------------------------------------------------------
// Dummy device / window (the vtable-copy source)
// ---------------------------------------------------------------------------

unsafe fn create_dummy_device(hwnd: HWND) -> Result<IDirect3DDevice9, String> {
    let d3d = Direct3DCreate9(D3D_SDK_VERSION).ok_or("Direct3DCreate9 failed")?;
    let mut params = D3DPRESENT_PARAMETERS {
        BackBufferWidth: 8,
        BackBufferHeight: 8,
        BackBufferFormat: D3DFORMAT(21), // D3DFMT_X8R8G8B8
        BackBufferCount: 1,
        SwapEffect: D3DSWAPEFFECT_DISCARD,
        hDeviceWindow: hwnd,
        Windowed: true.into(),
        PresentationInterval: 0x8000_0000, // D3DPRESENT_INTERVAL_IMMEDIATE
        ..Default::default()
    };
    let mut device: Option<IDirect3DDevice9> = None;
    d3d.CreateDevice(
        0, // D3DADAPTER_DEFAULT
        D3DDEVTYPE_HAL,
        hwnd,
        0x40, // D3DCREATE_HARDWARE_VERTEXPROCESSING
        &mut params,
        &mut device,
    )
    .map_err(|e| format!("CreateDevice: {e}"))?;
    device.ok_or_else(|| "no d3d9 device returned".to_string())
}

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
        lpszClassName: w!("iFrameD3D9Wnd"),
        style: CS_HREDRAW | CS_VREDRAW,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }
    let hinstance =
        unsafe { GetModuleHandleW(PCWSTR::null()).map_err(|e| format!("GetModuleHandleW: {e}"))? };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("iFrameD3D9Wnd"),
            w!("iFrameD3D9"),
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