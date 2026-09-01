//! iframe_hook.dll — injected into the game process.
//!
//! M1: DXGI `IDXGISwapChain::Present` / `Present1` vtable hooks (pass-through,
//! telemetry only). The JIT pacer integration lands in M2.
//!
//! Concurrency notes:
//! * `DllMain` only spawns the init thread (loader lock safety).
//! * `install()` runs once; a second load of the DLL is a no-op.
//! * The Present hot path is lock-free: QPC + ring push + trampoline call.

// The hook is unsafe-by-nature (vtable patching, raw COM calls); every
// `unsafe fn` here is a documented trust boundary, and wrapping each operation
// in nested `unsafe {}` blocks adds noise without safety. Revisit when this
// lint becomes a hard error in a future Rust edition.
#![allow(unsafe_op_in_unsafe_fn)]

pub mod engine;
pub mod hooks;
pub mod telemetry;
pub mod timing;
pub mod vblank;

use std::sync::atomic::{AtomicU32, Ordering};
use windows::core::BOOL;
use windows::Win32::Foundation::{HINSTANCE, HMODULE};
use windows::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// 0 = initialising, 1 = hooks installed, 2 = init failed.
pub static HOOK_STATE: AtomicU32 = AtomicU32::new(0);

/// DLL entry point. Keep it minimal: spawn a worker and return immediately.
#[unsafe(no_mangle)]
extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            unsafe {
                let _ = DisableThreadLibraryCalls(HMODULE(hinst.0));
            }
            // The thread will not actually run until DllMain returns (loader
            // lock), which is exactly what we want.
            std::thread::spawn(|| {
                if hooks::install() {
                    HOOK_STATE.store(1, Ordering::Release);
                } else {
                    HOOK_STATE.store(2, Ordering::Release);
                }
            });
            BOOL(1)
        }
        DLL_PROCESS_DETACH => {
            hooks::uninstall();
            BOOL(1)
        }
        _ => BOOL(1),
    }
}

/// Append-only debug log (init path only — never from the hot path).
pub fn log_line(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("iframe_hook.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", std::process::id(), msg);
    }
}

/// Fine-grained step logger for debugging the first pacing attempts.
/// Logs only the first ~40 steps, then goes silent (hot path stays clean).
pub fn debug_step(msg: &str) {
    use std::sync::atomic::AtomicU32;
    static STEPS: AtomicU32 = AtomicU32::new(0);
    let n = STEPS.fetch_add(1, Ordering::Relaxed);
    if n < 40 {
        log_line(&format!("step {n}: {msg}"));
    }
}

/// Exported probe used by the injector to verify the DLL loaded and runs.
/// Returns the shared-memory magic ("IFRM").
#[unsafe(no_mangle)]
pub extern "system" fn iframe_ping() -> u32 {
    0x4946_524D
}

/// Exported probe returning the hook ABI version.
#[unsafe(no_mangle)]
pub extern "system" fn iframe_abi_version() -> u32 {
    1
}