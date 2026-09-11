pub mod d3d9;
pub mod detour;
pub mod dxgi;
pub mod opengl;
pub mod vulkan;

/// Install all hooks. Returns true on success (sets HOOK_STATE in lib.rs).
pub fn install() -> bool {
    if !crate::telemetry::init() {
        crate::log_line("telemetry init failed");
        return false;
    }

    let mut ok = match unsafe { dxgi::install() } {
        Ok(()) => true,
        Err(e) => {
            crate::log_line(&format!("dxgi hook install failed: {e}"));
            false
        }
    };

    // D3D9 is opportunistic
    if let Err(e) = unsafe { d3d9::install() } {
        crate::log_line(&format!("d3d9 hooks skipped: {e}"));
    } else {
        ok = true;
    }

    // Vulkan is opportunistic (present if game loaded vulkan-1.dll)
    if let Err(e) = unsafe { vulkan::install() } {
        crate::log_line(&format!("vulkan hooks skipped: {e}"));
    } else {
        ok = true;
    }

    // OpenGL is opportunistic (present if game loaded opengl32.dll / gdi32.dll)
    if let Err(e) = unsafe { opengl::install() } {
        crate::log_line(&format!("opengl hooks skipped: {e}"));
    } else {
        ok = true;
    }

    // Publish the state through the shared header — the control app polls it
    // there (the process-local HOOK_STATE atomic is invisible across processes).
    if let Some(ring) = crate::telemetry::ring() {
        ring.set_hook_state(if ok { 1 } else { 2 });
    }
    ok
}

/// Best-effort cleanup (process exit makes this mostly moot, but a clean
/// unload must not leave hooked vtables behind).
pub fn uninstall() {
    dxgi::uninstall();
    d3d9::uninstall();
    vulkan::uninstall();
    opengl::uninstall();
}