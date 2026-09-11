//! All In Frame Hook Engine: Injected DLL for DXGI / D3D11 Asynchronous Mouse Warp.

pub mod depth;
pub mod hooks;
pub mod raw_input;
pub mod warp_engine;

use std::ffi::c_void;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Graphics::Direct3D11::ID3D11DepthStencilView;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

use aif_common::WarpConfig;
use depth::GLOBAL_DEPTH_TRACKER;
use hooks::{install_hooks, update_config};

#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    _hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        std::thread::spawn(|| {
            // Give host process time to initialize its DirectX runtime
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = install_hooks();
        });
    }
    1
}

/// Explicit initialization API.
#[no_mangle]
pub unsafe extern "C" fn aif_init() -> i32 {
    match install_hooks() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Dynamic configuration API.
#[no_mangle]
pub unsafe extern "C" fn aif_set_config(cfg: *const WarpConfig) -> i32 {
    if cfg.is_null() {
        return -1;
    }
    update_config(*cfg);
    0
}

/// Direct depth view registration API for custom engine integration.
#[no_mangle]
pub unsafe extern "C" fn aif_register_depth(dsv_raw: *mut c_void) -> i32 {
    if dsv_raw.is_null() {
        return -1;
    }
    let dsv = std::mem::ManuallyDrop::new(std::mem::transmute::<*mut c_void, ID3D11DepthStencilView>(dsv_raw));
    if let Ok(res) = dsv.GetResource() {
        if let Ok(mut tracker) = GLOBAL_DEPTH_TRACKER.lock() {
            tracker.register_depth_resource(res);
            return 0;
        }
    }
    -1
}
