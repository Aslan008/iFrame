//! DLL injection: classic `CreateRemoteThread` + `LoadLibraryW`.
//!
//! Safety posture:
//! * 32-bit targets get the i686 DLL build (`default_dll_path_for`).
//! * Refuses known anti-cheat processes (M6; the list lives in the app).
//! * The DLL path is canonicalised before being written into the target.

use std::ffi::c_void;
use std::path::Path;
use windows::core::{s, w, BOOL, PCWSTR};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{VirtualAllocEx, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, IsWow64Process, OpenProcess, WaitForSingleObject,
    PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};

/// Inject `dll_path` into process `pid` (x64 only in M1).
pub fn inject(pid: u32, dll_path: &Path) -> Result<(), String> {
    // M6 safety gate: refuse anti-cheat protected processes outright.
    crate::anticheat::check_process(pid)?;
    unsafe {
        let process = OpenProcess(
            PROCESS_CREATE_THREAD
                | PROCESS_QUERY_INFORMATION
                | PROCESS_VM_OPERATION
                | PROCESS_VM_WRITE
                | PROCESS_VM_READ,
            false,
            pid,
        )
        .map_err(|e| format!("OpenProcess({pid}): {e} (elevated target? run as admin)"))?;

        let path = std::fs::canonicalize(dll_path)
            .map_err(|e| format!("canonicalize dll path: {e}"))?
            .to_string_lossy()
            .into_owned();
        let mut wide: Vec<u16> = path.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * 2;

        let remote = VirtualAllocEx(process, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote.is_null() {
            return Err("VirtualAllocEx failed".into());
        }
        WriteProcessMemory(process, remote, wide.as_ptr() as *const c_void, bytes, None)
            .map_err(|e| format!("WriteProcessMemory: {e}"))?;

        let kernel32 = GetModuleHandleW(w!("kernel32.dll")).map_err(|e| e.to_string())?;
        // GetProcAddress returns FARPROC = Option<unsafe extern "system" fn() -> isize>;
        // unwrap it to the raw fn pointer, then transmute to the thread entry.
        let load_library = GetProcAddress(kernel32, s!("LoadLibraryW"))
            .ok_or("GetProcAddress(LoadLibraryW) failed")?;

        let thread = CreateRemoteThread(
            process,
            None,
            0,
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                ThreadFn,
            >(load_library)),
            Some(remote),
            0,
            None,
        )
        .map_err(|e| format!("CreateRemoteThread: {e}"))?;
        let _ = WaitForSingleObject(thread, 10_000);

        let mut code = 0u32;
        let _ = GetExitCodeThread(thread, &mut code);
        if code == 0 {
            return Err("LoadLibraryW failed inside the target".into());
        }
        let _ = windows::Win32::Foundation::CloseHandle(thread);
        let _ = windows::Win32::Foundation::CloseHandle(process);
        Ok(())
    }
}

type ThreadFn = unsafe extern "system" fn(*mut c_void) -> u32;

/// Is `pid` a 32-bit process (on this 64-bit OS)?
pub fn is_wow64(pid: u32) -> Result<bool, String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid)
            .map_err(|e| format!("OpenProcess({pid}): {e}"))?;
        let mut wow64 = BOOL(0);
        let r = IsWow64Process(process, &mut wow64).map_err(|e| format!("IsWow64Process: {e}"));
        let _ = windows::Win32::Foundation::CloseHandle(process);
        r?;
        Ok(wow64.as_bool())
    }
}

/// Default hook-DLL path for a target: checks next to the running executable,
/// then target/release, target/debug, and workspace root.
pub fn default_dll_path_for(pid: u32) -> std::path::PathBuf {
    let dll_name = if is_wow64(pid).unwrap_or(false) {
        "iframe_hook32.dll"
    } else {
        "iframe_hook.dll"
    };

    // 1. Next to the running executable (release or debug folder):
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(dll_name);
            if p.exists() {
                return p;
            }
        }
    }

    // 2. Standard workspace build locations:
    let candidates = [
        format!("target/release/{dll_name}"),
        format!("target/debug/{dll_name}"),
        format!("target/i686-pc-windows-msvc/release/{dll_name}"),
        format!("target/i686-pc-windows-msvc/debug/{dll_name}"),
        dll_name.to_string(),
    ];

    for c in &candidates {
        let p = std::path::PathBuf::from(c);
        if p.exists() {
            return p;
        }
    }

    // Default fallback
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(dll_name);
        }
    }
    std::path::PathBuf::from(format!("target/release/{dll_name}"))
}

/// Resolve the PID of a top-level window by exact title.
pub fn pid_from_window_title(title: &str) -> Result<u32, String> {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
    let wide: Vec<u16> = format!("{title}\0").encode_utf16().collect();
    unsafe {
        let hwnd = FindWindowW(PCWSTR::null(), PCWSTR::from_raw(wide.as_ptr()))
            .map_err(|e| format!("FindWindowW({title}): {e}"))?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return Err(format!("window '{title}' found but has no pid"));
        }
        Ok(pid)
    }
}