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
        Some("tune") => cmd_tune(&args[1..]),
        Some("solve-cadence") => cmd_solve_cadence(&args[1..]),
        Some("optimize-config") => cmd_optimize_config(&args[1..]),
        Some("--help") | Some("-h") | Some("help") => print_usage(),
        Some("--version") | Some("-V") => {
            println!("iFrame v{}", env!("CARGO_PKG_VERSION"));
        }
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
    // CLI one-shot: no interactive host will maintain heartbeats. Clear a
    // stale host_present left by a previous UI session, or the in-game
    // watchdog would silently ignore this config after 3 s.
    mapping.ring.mark_headless();
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

    // --hold: stay resident and maintain the host heartbeat so the in-game
    // watchdog keeps the limiter enabled. Ctrl+C releases the cap (a killed
    // holder is exactly the crash case the watchdog exists for).
    if enabled && args.iter().any(|a| a == "--hold") {
        println!("holding heartbeat for pid {pid} — Ctrl+C to release the limiter");
        unsafe {
            let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(
                Some(console_ctrl),
                true,
            );
        }
        while !CONSOLE_STOP.load(Ordering::Relaxed) {
            mapping.ring.update_heartbeat(qpc_now_u64());
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        mapping.ring.set_config(&RuntimeConfig { enabled: false, ..cfg });
        println!("limiter released.");
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

static CONSOLE_STOP: AtomicBool = AtomicBool::new(false);

/// Console Ctrl handler: flip the stop flag so long-running CLI modes (ETW
/// watch, `limit --hold`) stop cleanly (returning TRUE also prevents the
/// default hard process kill).
unsafe extern "system" fn console_ctrl(_ctrl: u32) -> windows::core::BOOL {
    CONSOLE_STOP.store(true, Ordering::SeqCst);
    windows::core::BOOL(1)
}

/// QPC counter as u64 (for shared-memory heartbeats).
fn qpc_now_u64() -> u64 {
    let mut v = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut v);
    }
    v as u64
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
            Some(console_ctrl),
            true,
        );
    }
    let mut last_total = 0u64;
    while !CONSOLE_STOP.load(Ordering::Relaxed) {
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

fn cmd_solve_cadence(args: &[String]) {
    use iframe_solver::cadence_synthesizer::CadenceSynthesizer;

    let fps: f64 = arg_value(args, "--fps").and_then(|v| v.parse().ok()).unwrap_or(40.0);
    let refresh: f64 = arg_value(args, "--refresh").and_then(|v| v.parse().ok()).unwrap_or(144.0);

    println!("Synthesizing discrete VBlank cadence schedule using C++ CDCL solver...");
    println!("Target FPS: {fps:.2}, Display Refresh: {refresh:.2} Hz");

    match CadenceSynthesizer::synthesize(fps, refresh) {
        Ok(res) => {
            println!("✔ CDCL Cadence Synthesis Success!");
            println!("  • Period length:      {} frames ({} VBlanks total)", res.period_frames, res.total_vblanks);
            println!("  • Avg VBlanks/frame:  {:.3}", res.avg_vblanks_per_frame);
            println!("  • Cadence step loop:  {:?}", res.steps);
        }
        Err(e) => {
            eprintln!("Cadence synthesis failed: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_optimize_config(args: &[String]) {
    use iframe_solver::config_optimizer::{ConfigOptimizer, SystemCapabilities, UserPreferences};

    let fps: f64 = arg_value(args, "--fps").and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let refresh: f64 = arg_value(args, "--refresh").and_then(|v| v.parse().ok()).unwrap_or(144.0);

    let caps = SystemCapabilities {
        has_flip_model: !args.iter().any(|a| a == "--no-flip"),
        monitor_vrr_capable: !args.iter().any(|a| a == "--no-vrr"),
        monitor_refresh_hz: refresh,
        is_exclusive_fullscreen: args.iter().any(|a| a == "--fullscreen"),
    };

    let prefs = UserPreferences {
        target_fps: fps,
        prefer_vrr: args.iter().any(|a| a == "--vrr") || (!args.iter().any(|a| a == "--fixed-vsync") && caps.monitor_vrr_capable),
        prefer_lowest_latency: true,
        allow_vsync_override: !args.iter().any(|a| a == "--no-vsync-override"),
    };

    println!("Synthesizing optimal SwapChain & Pacer configuration using C++ CDCL solver...");
    match ConfigOptimizer::optimize(&caps, &prefs) {
        Ok(cfg) => {
            println!("✔ Conflict-Free Runtime Configuration Synthesized:");
            println!("  • Pacer Enabled:   {}", cfg.enabled);
            println!("  • Pacer Mode:      {:?}", cfg.mode);
            println!("  • Target FPS:      {:.1}", cfg.target_fps);
            println!("  • Refresh Rate:    {:.1} Hz", cfg.refresh_hz);
            println!("  • VSync Override:  {}", cfg.vsync_override);
            println!("  • Force Waitable:  {}", cfg.force_waitable);
        }
        Err(e) => {
            eprintln!("Config optimization failed: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_tune(args: &[String]) {
    use iframe_common::config::RuntimeConfig;
    use iframe_common::pacer::PacerMode;
    use iframe_solver::telemetry_tuner::{GameTelemetryInput, SideMetrics, TelemetryTuner};

    let Some(pid) = arg_value(args, "--pid").and_then(|v| v.parse().ok()) else {
        eprintln!("tune: --pid <N> required");
        std::process::exit(2);
    };
    let refresh: f64 = arg_value(args, "--refresh").and_then(|v| v.parse().ok()).unwrap_or(144.0);

    let mapping = match sm_host::open(pid) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("tune: {e} (inject first)");
            std::process::exit(1);
        }
    };
    // CLI one-shot: clear a stale host_present from a previous UI session,
    // otherwise the watchdog would ignore the configs published below.
    mapping.ring.mark_headless();

    let mut freq = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
    }
    let freq = if freq <= 0 { 10_000_000 } else { freq };
    println!("🎯 Запуск CDCL Авто-Диагноста & Тюнера на PID {pid}...");
    println!("Шаг 1/2: Замер базового рендеринга без лимитера (3 сек)...");
    mapping.ring.set_config(&RuntimeConfig {
        enabled: false,
        mode: PacerMode::Bypass,
        target_fps: 0.0,
        refresh_hz: 0.0,
        vsync_override: false,
        force_waitable: false,
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    let r_off = bench::collect_phase(&mapping.ring, "OFF Baseline", 3, freq);

    println!("Шаг 2/2: Замер с лимитером (3 сек)...");
    mapping.ring.set_config(&RuntimeConfig {
        enabled: true,
        mode: PacerMode::FixedVsync,
        target_fps: 60.0,
        refresh_hz: refresh,
        vsync_override: true,
        force_waitable: false,
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    let r_on = bench::collect_phase(&mapping.ring, "ON Paced", 3, freq);

    let off_m = SideMetrics {
        fps: r_off.fps,
        p50_ms: r_off.ft_p50_ms,
        p99_ms: r_off.ft_p99_ms,
        jitter_ms: r_off.ft_jitter_ms,
        hold_p50_ms: r_off.present_hold_p50_ms,
        hold_max_ms: r_off.present_hold_max_ms,
        wait_p50_ms: r_off.pacing_wait_p50_ms,
        late_count: r_off.late_frames,
        sample_count: r_off.total_frames,
    };
    let on_m = SideMetrics {
        fps: r_on.fps,
        p50_ms: r_on.ft_p50_ms,
        p99_ms: r_on.ft_p99_ms,
        jitter_ms: r_on.ft_jitter_ms,
        hold_p50_ms: r_on.present_hold_p50_ms,
        hold_max_ms: r_on.present_hold_max_ms,
        wait_p50_ms: r_on.pacing_wait_p50_ms,
        late_count: r_on.late_frames,
        sample_count: r_on.total_frames,
    };

    let input = GameTelemetryInput {
        off: off_m,
        on: on_m,
        refresh_hz: refresh,
        current_target_fps: 60.0,
    };

    println!("\nРешение SAT-задачи оптимизации игрового конвейера...");
    match TelemetryTuner::analyze(&input) {
        Ok(rec) => {
            println!("✔ CDCL Анализ и Оптимизация завершены!");
            println!("  • Диагноз игры:         {}", rec.diagnosis_summary);
            println!("  • Рекомендованный FPS:  {:.0} FPS", rec.recommended_fps);
            println!("  • Режим пейсера:        {:?}", rec.recommended_mode);
            println!("  • Шаги каденции:        {:?}", rec.cadence_steps);
            println!("  • Снижение инпут-лага:  ~{:.1} мс", rec.expected_latency_reduction_ms);
            println!("  • Устранение джиттера:  в {:.1}x раз", rec.expected_jitter_improvement_times);
            println!("  • Разгрузка GPU:        {:.0}%", rec.expected_gpu_load_relief_percent);

            mapping.ring.set_config(&RuntimeConfig {
                enabled: true,
                mode: rec.recommended_mode,
                target_fps: rec.recommended_fps,
                refresh_hz: refresh,
                vsync_override: rec.recommended_vsync_override,
                force_waitable: false,
            });
            println!("\n✔ Оптимальный профиль CDCL применён к PID {pid}!");
        }
        Err(e) => {
            eprintln!("Авто-тюнинг не удался: {e}");
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    println!("iFrame v{} — zero-added-latency frame pacer (CDCL solver powered)", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage:");
    println!("  iframe [ui]");
    println!("  iframe list");
    println!("  iframe inject --pid <N> | --window <title> [--dll <path>]");
    println!("  iframe watch  --pid <N> [--seconds <S>]");
    println!("  iframe watch-etw --pid <N>            (telemetry-only, no injection)");
    println!("  iframe limit  --pid <N> --fps <F> [--mode vsync|vrr|off] [--refresh <Hz>]");
    println!("                [--no-vsync-override] [--waitable] [--hold]");
    println!("  iframe tune   --pid <N> [--refresh <Hz>]");
    println!("  iframe bench");
    println!("  iframe solve-cadence   --fps <F> [--refresh <Hz>]");
    println!("  iframe optimize-config --fps <F> [--refresh <Hz>] [--fullscreen] [--no-vrr]");
}