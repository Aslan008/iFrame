//! Anti-cheat protection: refuse injection into protected processes.
//!
//! Online games run kernel-level anti-cheats (EAC, BattlEye, Vanguard, ...).
//! Injecting a DLL into them risks a permanent account ban — iFrame refuses
//! outright and offers the telemetry-only ETW mode instead.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Process32FirstW, Process32NextW,
    MODULEENTRY32W, PROCESSENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPPROCESS,
};

/// Process-image names (lowercase, no path) of known anti-cheat services.
const PROTECTED_PROCESSES: &[&str] = &[
    // Easy Anti-Cheat
    "easyanticheat.exe",
    "easyanticheat_sys.exe",
    "easyanticheatsetup.exe",
    // BattlEye
    "beservice.exe",
    "beserver.exe",
    "beshellsvc.exe",
    "bedaisy.exe",
    // Riot Vanguard
    "vgc.exe",
    "vgtray.exe",
    "vgk.exe",
    // FACEIT / ESEA
    "faceitclient.exe",
    "faceit.exe",
    "eseaclient.exe",
    // Misc anti-cheat services
    "fairplaykd.exe",
    "mhyprot2.exe",
    "atprotect.exe",
    "xigncode3.exe",
    "gameguard.exe",
    "weagle.exe",
    "netsh.exe",
];

/// Module names (lowercase) that indicate an anti-cheat is loaded into the
/// target process. Injection is refused if ANY module matches.
const PROTECTED_MODULES: &[&str] = &[
    "easyanticheat.dll",
    "easyanticheat_x64.dll",
    "easyanticheat_x86.dll",
    "easyanticheat.sys",
    "battleye.dll",
    "battleye_x64.dll",
    "battleye.sys",
    "vgk.dll",
    "vgk.sys",
    "faceitclient.dll",
    "esea.sys",
    "mhyprot2.sys",
    "mhyprot3.sys",
    "xigncode.dll",
    "xigncode3.dll",
    "atprotect.dll",
    "gameguard.dll",
];

/// Known protected game executables (incomplete — extend as needed).
const PROTECTED_GAMES: &[&str] = &[
    "valorant.exe",
    "fortniteclient-win64-shipping.exe",
    "rustclient.exe",
    "escapefromtarkov.exe",
    "r5apex.exe",
    "apex_legends.exe",
    "cod.exe",
    "modernwarfare.exe",
    "blackops4.exe",
    "gta5_enhanced.exe",
    "gta5.exe",
    "pubg.exe",
    "tslgame.exe",
    "rainbowsix.exe",
    "vfs.exe",
    "destiny2.exe",
    "genshinimpact.exe",
    "yuanshen.exe",
    "zenlesszonezero.exe",
    "huntshowdown.exe",
    "dayz_x64.exe",
    "arma3_x64.exe",
    "smite.exe",
    "splitgate.exe",
    "newworld.exe",
];

/// Is this process-image name on the protected list?
pub fn is_protected_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    PROTECTED_PROCESSES.iter().any(|p| *p == n) || PROTECTED_GAMES.iter().any(|g| *g == n)
}

/// Full check: the process image name + its loaded modules. Returns `Err`
/// with a refusal message when the target is protected.
pub fn check_process(pid: u32) -> Result<(), String> {
    // 1. the process image name
    let name = process_name(pid)?;
    if is_protected_name(&name) {
        return Err(format!(
            "REFUSED: '{name}' is an anti-cheat protected process — injection \
             risks a permanent ban. Use telemetry-only mode instead."
        ));
    }

    // 2. loaded modules (an anti-cheat DLL can be loaded into ANY game)
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid) }
        .map_err(|e| format!("module snapshot: {e}"))?;
    let mut entry = MODULEENTRY32W {
        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe {
        if Module32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szModule[..entry.szModule.iter().position(|&c| c == 0).unwrap_or(0)],
                );
                let lower = name.to_ascii_lowercase();
                if PROTECTED_MODULES.iter().any(|p| *p == lower) {
                    let _ = CloseHandle(snapshot);
                    return Err(format!(
                        "REFUSED: anti-cheat module '{name}' detected in pid {pid} — \
                         injection risks a permanent ban. Use telemetry-only mode instead."
                    ));
                }
                if Module32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(())
}

/// The process image name (e.g. "game.exe") for a PID.
pub fn process_name(pid: u32) -> Result<String, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| format!("process snapshot: {e}"))?;
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut name = None;
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    name = Some(String::from_utf16_lossy(
                        &entry.szExeFile
                            [..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)],
                    ));
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    name.ok_or_else(|| format!("pid {pid} not found"))
}