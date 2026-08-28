//! iframe_hook.dll — injected into the game process.
//!
//! M0: build/link smoke test only. The DXGI/D3D9 vtable hooks, the JIT pacer
//! integration and the telemetry writer land in M1–M2.

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