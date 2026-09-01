//! Minimal D3D11 "game" for testing the iFrame hook end-to-end.
//!
//! Opens a window and presents frames as fast as possible (or vsync-capped),
//! with an optional artificial CPU cost per frame — a controllable stand-in
//! for a real game.
//!
//! Usage: d3d11_test_app [--cpu-ms <f32>] [--vsync <0|1>] [--seconds <N>]
//!                       [--width <W>] [--height <H>]

use std::io::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISwapChain1, DXGI_PRESENT,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, DXGI_SCALING_NONE,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::Sleep;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PostQuitMessage,
    RegisterClassW, SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG,
    PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_QUIT,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

static INGAME_CAP_BITS: AtomicU64 = AtomicU64::new(0);
static LAST_CLICK_QPC: AtomicI64 = AtomicI64::new(0);

fn set_cap(fps: f64) {
    INGAME_CAP_BITS.store(fps.to_bits(), Ordering::Relaxed);
}

fn get_cap() -> f64 {
    f64::from_bits(INGAME_CAP_BITS.load(Ordering::Relaxed))
}

struct Args {
    cpu_ms: f64,
    vsync: u32,
    seconds: Option<u64>,
    width: u32,
    height: u32,
    tearing: bool,
    delay: u64,
    cap_fps: f64,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let get = |i: usize| argv.get(i).cloned();
    let mut cfg = Args {
        cpu_ms: 0.0,
        vsync: 0,
        seconds: None,
        width: 640,
        height: 360,
        tearing: false,
        delay: 0,
        cap_fps: 0.0,
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--cpu-ms" => cfg.cpu_ms = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--vsync" => cfg.vsync = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0),
            "--seconds" => cfg.seconds = get(i + 1).and_then(|v| v.parse().ok()),
            "--width" => cfg.width = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(640),
            "--height" => cfg.height = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(360),
            "--tearing" => cfg.tearing = true,
            "--delay" => cfg.delay = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0),
            "--cap-fps" => cfg.cap_fps = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            _ => {}
        }
        i += 1;
    }
    cfg
}

fn main() {
    let args = parse_args();
    set_cap(args.cap_fps);
    println!("PID={}", std::process::id());
    println!(
        "d3d11_test_app: {}x{}, vsync={}, cpu_ms={:.2}, tearing={}, in-game-cap={:.1}",
        args.width, args.height, args.vsync, args.cpu_ms, args.tearing, args.cap_fps
    );
    println!("Controls: [Space]/[L] = Toggle In-Game Limiter (60 FPS vs Uncapped), [Up]/[Down] = Adjust Cap");
    let _ = std::io::stdout().flush();
    run_window(args);
}

fn run_window(args: Args) {
    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            match msg {
                WM_DESTROY => {
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN => {
                    let mut now = 0i64;
                    let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now);
                    LAST_CLICK_QPC.store(now, Ordering::Relaxed);
                    return LRESULT(0);
                }
                WM_KEYDOWN => {
                    let key = wparam.0 as i32;
                    if key == 0x20 || key == 0x4C { // Space or 'L'
                        let cur = get_cap();
                        let next = if cur > 0.0 { 0.0 } else { 60.0 };
                        set_cap(next);
                        println!("In-game limiter toggled: {:.1} FPS", next);
                    } else if key == 0x26 { // Up arrow
                        let cur = get_cap();
                        let next = (cur + 10.0).clamp(10.0, 360.0);
                        set_cap(next);
                        println!("In-game cap set to: {:.1} FPS", next);
                    } else if key == 0x28 { // Down arrow
                        let cur = get_cap();
                        let next = (cur - 10.0).max(10.0);
                        set_cap(next);
                        println!("In-game cap set to: {:.1} FPS", next);
                    }
                    return LRESULT(0);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: w!("iFrameTestWnd"),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hinstance = GetModuleHandleW(PCWSTR::null()).expect("GetModuleHandleW");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("iFrameTestWnd"),
            w!("iFrame Test D3D11"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            args.width as i32,
            args.height as i32,
            None,
            None,
            Some(HINSTANCE(hinstance.0)),
            None,
        )
        .expect("CreateWindowExW");
        let _ = ShowWindow(hwnd, SW_SHOW);
        // Foreground: DWM composes background windows at a throttled rate
        // (~64 Hz), which would quantize presents regardless of tearing.
        // A real game runs in the foreground — mirror that here.
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);

        // Optional delay BEFORE swap chain creation — the injector uses it to
        // publish a config (force_waitable) first and watch the factory hook
        // apply it to a swap chain created afterwards.
        if args.delay > 0 {
            println!("delaying swap chain creation by {}s ...", args.delay);
            let _ = std::io::stdout().flush();
            Sleep((args.delay * 1000) as u32);
        }

        // Modern creation path: the swap chain goes through the factory's
        // CreateSwapChainForHwnd — exactly what the iFrame factory hook sees.
        let mut device_opt: Option<ID3D11Device> = None;
        let mut context_opt: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device_opt),
            None,
            Some(&mut context_opt),
        )
        .expect("D3D11CreateDevice");
        let device = device_opt.expect("device");
        let context = context_opt.expect("context");

        let dxgi_device: IDXGIDevice = device.cast().expect("cast IDXGIDevice");
        let adapter = dxgi_device.GetAdapter().expect("GetAdapter");
        let factory: IDXGIFactory2 = adapter.GetParent().expect("GetParent factory");

        let desc1 = windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_DESC1 {
            Width: args.width,
            Height: args.height,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if args.tearing {
                DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
            } else {
                0
            },
        };
        let swapchain: IDXGISwapChain1 = factory
            .CreateSwapChainForHwnd(&device, hwnd, &desc1, None, None::<&IDXGIOutput>)
            .expect("CreateSwapChainForHwnd");

        let backbuffer: ID3D11Texture2D = swapchain.GetBuffer(0).expect("GetBuffer");
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        device
            .CreateRenderTargetView(&backbuffer, None, Some(&mut rtv))
            .expect("CreateRenderTargetView");
        let rtv = rtv.expect("rtv");
        context.OMSetRenderTargets(
            Some(&[Some(rtv.clone())]),
            None::<&windows::Win32::Graphics::Direct3D11::ID3D11DepthStencilView>,
        );

        println!("READY"); // marker for the test harness
        let _ = std::io::stdout().flush();

        let start = std::time::Instant::now();
        let mut msg = MSG::default();
        let mut phase = 0.0f32;
        let mut last_frame = std::time::Instant::now();
        let mut last_title_update = std::time::Instant::now();
        let mut frames_count = 0u32;
        let mut last_fps_time = std::time::Instant::now();
        let mut current_fps = 0.0f64;
        let mut current_ft = 0.0f64;
        let _ = current_ft;
        let mut target_qpc = 0i64;

        loop {
            // Pump messages.
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Some(secs) = args.seconds {
                if start.elapsed().as_secs_f64() > secs as f64 {
                    break;
                }
            }
            // Simulated CPU frame cost.
            if args.cpu_ms > 0.0 {
                Sleep(args.cpu_ms as u32);
            }
            // Render: cycling clear colour (visible proof of life).
            phase += 0.01;
            let color = [0.5 + 0.5 * phase.sin(), 0.5, 0.5 - 0.5 * phase.sin(), 1.0];
            context.ClearRenderTargetView(&rtv, &color);

            // Classic In-Game Limiter (Absolute QPC Anchor — holds the frame before Present)
            let cap = get_cap();
            if cap > 0.0 {
                let mut freq = 0i64;
                let mut now_qpc = 0i64;
                let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
                let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now_qpc);
                if freq > 0 {
                    let interval_ticks = (freq as f64 / cap).round() as i64;
                    if target_qpc == 0 || (now_qpc - target_qpc).abs() > interval_ticks * 2 {
                        target_qpc = now_qpc + interval_ticks;
                    } else {
                        target_qpc += interval_ticks;
                    }
                    while now_qpc < target_qpc {
                        std::hint::spin_loop();
                        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now_qpc);
                    }
                }
            } else {
                target_qpc = 0;
            }

            let present_flags = if args.tearing && args.vsync == 0 {
                windows::Win32::Graphics::Dxgi::DXGI_PRESENT_ALLOW_TEARING
            } else {
                DXGI_PRESENT(0)
            };
            let hr = swapchain.Present(args.vsync, present_flags);
            if hr.is_err() {
                eprintln!("Present failed: {:?}", hr);
                break;
            }

            let now = std::time::Instant::now();
            current_ft = now.duration_since(last_frame).as_secs_f64() * 1000.0;
            last_frame = now;
            frames_count += 1;

            if last_fps_time.elapsed().as_secs_f64() >= 0.5 {
                current_fps = frames_count as f64 / last_fps_time.elapsed().as_secs_f64();
                frames_count = 0;
                last_fps_time = std::time::Instant::now();
            }

            if last_title_update.elapsed().as_secs_f64() >= 0.1 {
                last_title_update = std::time::Instant::now();
                let cap_str = if cap > 0.0 {
                    format!("{:.0} FPS [Classic Cap]", cap)
                } else {
                    "OFF [Uncapped]".to_string()
                };
                let click_qpc = LAST_CLICK_QPC.load(Ordering::Relaxed);
                let click_str = if click_qpc > 0 {
                    let mut freq = 0i64;
                    let mut now_qpc = 0i64;
                    let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
                    let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now_qpc);
                    if freq > 0 {
                        let lat = (now_qpc - click_qpc) as f64 * 1000.0 / freq as f64;
                        format!(" | Click Lag: {:.1}ms", lat)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                let title = format!(
                    "iFrame Test [PID: {}] | {:.1} FPS ({:.1}ms) | In-Game Cap: {}{}\0",
                    std::process::id(), current_fps, current_ft, cap_str, click_str
                );
                let title_u16: Vec<u16> = title.encode_utf16().collect();
                let _ = SetWindowTextW(hwnd, PCWSTR(title_u16.as_ptr()));
            }
        }
    }
}