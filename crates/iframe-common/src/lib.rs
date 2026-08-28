//! iFrame common: pure-Rust math and IPC protocol shared by the injected DLL
//! and the control app. No WinAPI dependencies here — everything is testable.

pub mod config;
pub mod pacer;
pub mod shared_mem;

/// Well-known prefix for per-process shared memory mappings:
/// `Local\iFrameSM_<pid>`.
pub const SM_NAME_PREFIX: &str = r"Local\iFrameSM_";