pub mod dxgi;

/// Install all hooks. Returns true on success (sets HOOK_STATE in lib.rs).
pub fn install() -> bool {
    if !crate::telemetry::init() {
        crate::log_line("telemetry init failed");
        return false;
    }
    match unsafe { dxgi::install() } {
        Ok(()) => true,
        Err(e) => {
            crate::log_line(&format!("dxgi hook install failed: {e}"));
            false
        }
    }
}

/// Best-effort cleanup (process exit makes this mostly moot, but a clean
/// unload must not leave hooked vtables behind).
pub fn uninstall() {
    dxgi::uninstall();
}