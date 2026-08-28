//! iframe.exe — control app CLI (M1: inject / watch / list).
//! The egui UI arrives in M3; the CLI is the automation surface for M1–M2.

mod injector;
mod sm_host;
mod watch;

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(),
        Some("inject") => cmd_inject(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        _ => print_usage(),
    }
}


fn cmd_list() {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    unsafe extern "system" fn cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len == 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            println!("{pid:>7}  {title}");
            BOOL(1)
        }
    }
    unsafe {
        if let Err(e) = EnumWindows(Some(cb), LPARAM(0)) {
            eprintln!("EnumWindows: {e}");
        }
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_inject(args: &[String]) {
    let dll = arg_value(args, "--dll")
        .map(PathBuf::from)
        .unwrap_or_else(default_dll_path);
    if !dll.exists() {
        eprintln!("DLL not found: {}", dll.display());
        std::process::exit(2);
    }
    let pid = match arg_value(args, "--pid") {
        Some(p) => p.parse().unwrap_or_else(|_| {
            eprintln!("--pid must be a number");
            std::process::exit(2);
        }),
        None => match arg_value(args, "--window") {
            Some(title) => match injector::pid_from_window_title(&title) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("inject requires --pid <N> or --window <title>");
                std::process::exit(2);
            }
        },
    };

    println!("creating shared memory for pid {pid} ...");
    let mapping = match sm_host::create_for_pid(pid) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shared memory: {e}");
            std::process::exit(1);
        }
    };
    let _ = mapping; // keep the mapping alive until injection completes

    println!("injecting {} into pid {pid} ...", dll.display());
    if let Err(e) = injector::inject(pid, &dll) {
        eprintln!("inject failed: {e}");
        std::process::exit(1);
    }

    // Wait for the hook to report readiness through the shared header.
    let mapping = match sm_host::open(pid) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("re-open mapping: {e}");
            std::process::exit(1);
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = mapping.ring.hook_state();
        match state {
            1 => {
                println!("OK: hooks installed, telemetry flowing.");
                break;
            }
            2 => {
                eprintln!("hook init FAILED inside target (see %TEMP%\\iframe_hook.log)");
                std::process::exit(1);
            }
            _ => {
                if std::time::Instant::now() > deadline {
                    eprintln!("timeout waiting for hook init (state={state})", state = mapping.ring.hook_state());
                    std::process::exit(3);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn default_dll_path() -> PathBuf {
    PathBuf::from("target/release/iframe_hook.dll")
}

fn cmd_watch(args: &[String]) {
    let Some(pid) = arg_value(args, "--pid").and_then(|v| v.parse().ok()) else {
        eprintln!("watch: --pid <N> required");
        std::process::exit(2);
    };
    let seconds: u64 = arg_value(args, "--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    if let Err(e) = watch::watch(pid, seconds) {
        eprintln!("watch failed: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    println!("iFrame v{} — zero-added-latency frame pacer", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage:");
    println!("  iframe list");
    println!("  iframe inject --pid <N> | --window <title> [--dll <path>]");
    println!("  iframe watch  --pid <N> [--seconds <S>]");
}