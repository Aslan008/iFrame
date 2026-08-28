//! Minimal D3D11 "game" for testing the iFrame hook end-to-end.
//!
//! Opens a window and presents frames as fast as possible (or vsync-capped),
//! with an optional artificial CPU cost per frame — a controllable stand-in
//! for a real game.
//!
//! Usage: d3d11_test_app [--cpu-ms <f32>] [--vsync <0|1>] [--seconds <N>]
//!                       [--width <W>] [--height <H>]

use std::io::Write;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_MODE_SCALING_UNSPECIFIED,
    DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::Sleep;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PostQuitMessage,
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE,
    SW_SHOW, WINDOW_EX_STYLE, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const WINDOW_TITLE: &str = "iFrame Test D3D11";

struct Args {
    cpu_ms: f64,
    vsync: u32,
    seconds: Option<u64>,
    width: u32,
    height: u32,
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
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--cpu-ms" => cfg.cpu_ms = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--vsync" => cfg.vsync = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0),
            "--seconds" => cfg.seconds = get(i + 1).and_then(|v| v.parse().ok()),
            "--width" => cfg.width = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(640),
            "--height" => cfg.height = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(360),
            _ => {}
        }
        i += 1;
    }
    cfg
}

fn main() {
    let args = parse_args();
    println!("PID={}", std::process::id());
    println!(
        "d3d11_test_app: {}x{}, vsync={}, cpu_ms={:.2}",
        args.width, args.height, args.vsync, args.cpu_ms
    );
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
            if msg == WM_DESTROY {
                PostQuitMessage(0);
                return LRESULT(0);
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

        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: args.width,
                Height: args.height,
                RefreshRate: DXGI_RATIONAL {
                    Numerator: 0,
                    Denominator: 0,
                },
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
                Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            Flags: 0,
        };
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut swapchain: Option<IDXGISwapChain> = None;
        D3D11CreateDeviceAndSwapChain(
            None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut swapchain),
            Some(&mut device),
            None, // pfeaturelevel out
            Some(&mut context),
        )
        .expect("D3D11CreateDeviceAndSwapChain");
        let device = device.expect("device");
        let context = context.expect("context");
        let swapchain = swapchain.expect("swapchain");

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
            let hr = swapchain.Present(args.vsync, DXGI_PRESENT(0));
            if hr.is_err() {
                eprintln!("Present failed: {:?}", hr);
                break;
            }
        }
    }
}