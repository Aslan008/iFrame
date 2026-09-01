//! iframe.exe — control app: GUI (default) + CLI (inject / watch / limit / list).

use iframe_app::{bench, etw, injector, live, sm_host, ui, watch};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("ui") => {
            if let Err(e) = ui::run() {
                eprintln!("UI exited: {e}");
                std::process::exit(1);
            }
        }
        Some("list") => cmd_list(),
        Some("inject") => cmd_inject(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        Some("watch-etw") => cmd_watch_etw(&args[1..]),
        Some("limit") => cmd_limit(&args[1..]),
        Some("bench") => bench::run_benchmark(),
        Some("--help") | Some("-h") | Some("help") => print_usage(),
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            std::process::exit(2);
        }
    }
}

/// `iframe limit --pid N --fps 40 [--mode vsync|vrr|off] [--refresh HZ]`
/// Publishes the runtime config into the game's shared header.
fn cmd_limit(args: &[String]) {
    use iframe_common::config::RuntimeConfig;
    use iframe_common::pacer::PacerMode;

    let Some(pid) = arg_value(args, "--pid").and_then(|v| v.parse().ok()) else {
        eprintln!("limit: --pid <N> required");
        std::process::exit(2);
    };
    let fps: f64 = arg_value(args, "--fps").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let mode = match arg_value(args, "--mode").as_deref() {
        Some("vrr") => PacerMode::Vrr,
        Some("off") => PacerMode::Bypass,
        _ => PacerMode::FixedVsync,
    };
    let refresh: f64 = arg_value(args, "--refresh").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let enabled = fps > 0.0 && mode != PacerMode::Bypass;

    let mapping = match sm_host::open(pid) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("limit: {e} (inject first)");
            std::process::exit(1);
        }
    };
    let cfg = RuntimeConfig {
        enabled,
        mode,
        target_fps: fps,
        refresh_hz: refresh,
        vsync_override: !args.iter().any(|a| a == "--no-vsync-override"),
        force_waitable: args.iter().any(|a| a == "--waitable"),
    };
    mapping.ring.set_config(&cfg);
    if enabled {
        println!("limit set: {:.0} FPS ({mode:?}) for pid {pid}", fps);
    } else {
        println!("limiter disabled for pid {pid}");
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
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_inject(args: &[String]) {
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

    // Pick the DLL matching the target's bitness (unless --dll overrides).
    let dll = match arg_value(args, "--dll") {
        Some(p) => PathBuf::from(p),
        None => injector::default_dll_path_for(pid),
    };
    if !dll.exists() {
        eprintln!(
            "DLL not found: {} (build it with: cargo build --release --target i686-pc-windows-msvc -p iframe-hook)",
            dll.display()
        );
        std::process::exit(2);
    }

    println!("creating shared memory for pid {pid} ...");
    // Held for the rest of the function: dropping it early would destroy the
    // section object before the injected DLL gets a chance to attach.
    let mapping = match sm_host::create_for_pid(pid) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shared memory: {e}");
            std::process::exit(1);
        }
    };

    println!("injecting {} into pid {pid} ...", dll.display());
    if let Err(e) = injector::inject(pid, &dll) {
        eprintln!("inject failed: {e}");
        std::process::exit(1);
    }
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

static ETW_STOP: AtomicBool = AtomicBool::new(false);

/// Console Ctrl handler: flip the stop flag so the trace is stopped cleanly
/// (returning TRUE also prevents the default hard process kill).
unsafe extern "system" fn etw_console_ctrl(_ctrl: u32) -> windows::core::BOOL {
    ETW_STOP.store(true, Ordering::SeqCst);
    windows::core::BOOL(1)
}

/// Telemetry-only observation via ETW — no injection, safe for anti-cheat
/// protected games. Prints live stats until Ctrl+C.
fn cmd_watch_etw(args: &[String]) {
    let Some(pid) = arg_value(args, "--pid").and_then(|v| v.parse().ok()) else {
        eprintln!("watch-etw: --pid <N> required");
        std::process::exit(2);
    };
    let state = live::SharedState::new();
    state
        .attached_pid
        .store(pid, std::sync::atomic::Ordering::Relaxed);
    let mut watch = match etw::start_watch(pid, state.clone()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("watch-etw failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "ETW telemetry-only watch on pid {pid} (no injection) — Ctrl+C to stop."
    );
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(
            Some(etw_console_ctrl),
            true,
        );
    }
    let mut last_total = 0u64;
    while !ETW_STOP.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let stats = state.stats.lock().unwrap();
        let recent = stats.total - last_total;
        last_total = stats.total;
        println!(
            "presents: {:5}  fps: {:6.1}  p50: {:7.0} µs",
            recent,
            recent as f64 / 2.0,
            stats.p50_us
        );
    }
    watch.stop();
    println!("ETW watch stopped.");
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
    println!("  iframe limit  --pid <N> --fps <F> [--mode vsync|vrr|off] [--refresh <Hz>]");
    println!("                [--no-vsync-override] [--waitable]");
}