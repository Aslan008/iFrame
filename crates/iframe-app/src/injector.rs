//! DLL injection: classic `CreateRemoteThread` + `LoadLibraryW`.
//!
//! Safety posture:
//! * Refuses 32-bit targets (x86 DLL is M5 scope).
//! * Refuses known anti-cheat processes (M6; the list lives in the app).
//! * The DLL path is canonicalised before being written into the target.

use std::ffi::c_void;
use std::path::Path;
use windows::core::{s, w, BOOL, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{VirtualAllocEx, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, IsWow64Process, OpenProcess, WaitForSingleObject,
    PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};

type ThreadStart = unsafe extern "system" fn(*mut c_void) -> u32;

/// Inject `dll_path` into process `pid` (x64 only in M1).
pub fn inject(pid: u32, dll_path: &Path) -> Result<(), String> {
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

        // x86 targets need the x86 DLL build (M5).
        let mut wow64 = BOOL(0);
        IsWow64Process(process, &mut wow64).map_err(|e| format!("IsWow64Process: {e}"))?;
        if wow64.as_bool() {
            return Err("target is a 32-bit process — x86 DLL support lands in M5".into());
        }

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

use windows::Win32::Foundation::FARPROC;
type ThreadFn = unsafe extern "system" fn(*mut c_void) -> u32;

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