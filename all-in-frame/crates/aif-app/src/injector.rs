//! Process Discovery & Remote DLL Injection for All In Frame.

use std::ffi::c_void;
use std::path::Path;

use windows::core::{s, w, BOOL};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, OpenProcess, WaitForSingleObject,
    PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub window_title: String,
    pub hwnd: usize,
}

pub fn list_running_game_windows() -> Vec<ProcessInfo> {
    let mut results: Vec<ProcessInfo> = Vec::new();

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<ProcessInfo>);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return BOOL(1);
        }

        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);
        if len > 0 {
            let title = String::from_utf16_lossy(&title_buf[..len as usize]);
            // Filter out default desktop / empty titles
            if !title.is_empty() && title != "Program Manager" && title != "Default IME" && title != "All In Frame" {
                let name = get_process_name_by_pid(pid).unwrap_or_else(|| "Unknown".to_string());
                list.push(ProcessInfo {
                    pid,
                    name,
                    window_title: title,
                    hwnd: hwnd.0 as usize,
                });
            }
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut results as *mut Vec<ProcessInfo> as isize),
        );
    }

    results
}

fn get_process_name_by_pid(pid: u32) -> Option<String> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    let _ = CloseHandle(snap);
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    return Some(String::from_utf16_lossy(&entry.szExeFile[..len]));
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    None
}

/// Injects the All In Frame hook DLL into the target process.
#[allow(dead_code)]
pub fn inject_dll(pid: u32, dll_path: &Path) -> Result<(), String> {
    if !dll_path.exists() {
        return Err(format!("DLL file not found: {}", dll_path.display()));
    }

    let canon = std::fs::canonicalize(dll_path).map_err(|e| format!("canonicalize dll path: {e}"))?;
    let path_str = canon.to_string_lossy();
    let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
    let mut dll_path_str: Vec<u16> = clean_path.encode_utf16().collect();
    dll_path_str.push(0);

    let byte_len = dll_path_str.len() * 2;

    unsafe {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_WRITE
            | PROCESS_VM_READ;
        let h_proc = OpenProcess(access, false, pid)
            .map_err(|e| format!("OpenProcess failed: {e}"))?;

        let remote_mem = VirtualAllocEx(
            h_proc,
            None,
            byte_len,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote_mem.is_null() {
            let _ = CloseHandle(h_proc);
            return Err("VirtualAllocEx failed in target process".into());
        }

        let mut written: usize = 0;
        let write_ok = WriteProcessMemory(
            h_proc,
            remote_mem,
            dll_path_str.as_ptr() as *const c_void,
            byte_len,
            Some(&mut written),
        );

        if write_ok.is_err() || written != byte_len {
            let _ = VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
            let _ = CloseHandle(h_proc);
            return Err("WriteProcessMemory failed".into());
        }

        let kernel32 = GetModuleHandleW(w!("kernel32.dll"))
            .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
        let load_lib = GetProcAddress(kernel32, s!("LoadLibraryW"))
            .ok_or("GetProcAddress(LoadLibraryW) failed")?;

        let h_thread = CreateRemoteThread(
            h_proc,
            None,
            0,
            Some(std::mem::transmute(load_lib)),
            Some(remote_mem),
            0,
            None,
        )
        .map_err(|e| format!("CreateRemoteThread failed: {e}"))?;

        let _ = WaitForSingleObject(h_thread, 5000);
        let mut exit_code = 0u32;
        let _ = windows::Win32::System::Threading::GetExitCodeThread(h_thread, &mut exit_code);
        let _ = CloseHandle(h_thread);
        let _ = VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
        let _ = CloseHandle(h_proc);

        if exit_code == 0 {
            return Err("LoadLibraryW вернула NULL внутри процесса (ошибка загрузки DLL)".into());
        }
    }

    Ok(())
}
