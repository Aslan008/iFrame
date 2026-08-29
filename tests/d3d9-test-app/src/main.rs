//! Minimal D3D9 present-loop stand for end-to-end iFrame testing.
//!
//! Creates a windowed D3D9 device (INTERVAL_IMMEDIATE ≈ vsync-off) and
//! presents in a loop. The iFrame hook (Present@17) sees these presents.
//!
//! Usage: d3d9_test_app [--seconds N] [--cpu-ms F] [--width W] [--height H]

use std::io::Write;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D9::{
    Direct3DCreate9, IDirect3DDevice9, D3DCREATE_HARDWARE_VERTEXPROCESSING, D3DDEVTYPE_HAL,
    D3DFORMAT, D3DPRESENT_PARAMETERS, D3DSWAPEFFECT_DISCARD, D3D_SDK_VERSION,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::Sleep;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, GetMessageW, PeekMessageW, RegisterClassW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WNDCLASSW,
    WM_QUIT, WS_OVERLAPPEDWINDOW,
};

const WINDOW_TITLE: &str = "iFrame Test D3D9";

struct Args {
    cpu_ms: f64,
    seconds: Option<u64>,
    width: u32,
    height: u32,
    delay_device: u64,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let get = |i: usize| argv.get(i).cloned();
    let mut cfg = Args {
        cpu_ms: 0.0,
        seconds: None,
        width: 640,
        height: 360,
        delay_device: 0,
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--cpu-ms" => cfg.cpu_ms = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--seconds" => cfg.seconds = get(i + 1).and_then(|v| v.parse().ok()),
            "--width" => cfg.width = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(640),
            "--height" => cfg.height = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(360),
            "--delay-device" => {
                cfg.delay_device = get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0)
            }
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
        "d3d9_test_app: {}x{}, cpu_ms={:.2}",
        args.width, args.height, args.cpu_ms
    );
    let _ = std::io::stdout().flush();
    run_window(args);
    println!("DONE");
    let _ = std::io::stdout().flush();
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn run_window(args: Args) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: w!("iFrameD3D9TestWnd"),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("iFrameD3D9TestWnd"),
            w!("iFrame Test D3D9"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            args.width as i32,
            args.height as i32,
            None,
            None,
            None,
            None,
        )
        .expect("CreateWindowExW");
        let _ = ShowWindow(hwnd, SW_SHOW);

        // Optional delay: emulate a game that creates its device some time
        // after process start — the iFrame IAT hook must catch it then.
        if args.delay_device > 0 {
            println!("delaying device creation by {}s ...", args.delay_device);
            let _ = std::io::stdout().flush();
            Sleep((args.delay_device * 1000) as u32);
        }
        // Diagnostic: read the Direct3DCreate9 IAT slot (base+0x211e0 in this
        // build) right before the call — proves whether the iFrame patch is
        // in place at call time.
        let exe_base = windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR::null())
            .expect("GetModuleHandleW");
        let slot = (exe_base.0 as usize + 0x211e0) as *const usize;
        println!(
            "IAT slot @ base+0x211e0 = {:#x} (right before the call)",
            std::ptr::read(slot)
        );
        let _ = std::io::stdout().flush();
        let d3d = Direct3DCreate9(D3D_SDK_VERSION).expect("Direct3DCreate9");
        let mut params = D3DPRESENT_PARAMETERS {
            BackBufferWidth: args.width,
            BackBufferHeight: args.height,
            BackBufferFormat: D3DFORMAT(21), // D3DFMT_X8R8G8B8
            BackBufferCount: 1,
            SwapEffect: D3DSWAPEFFECT_DISCARD,
            hDeviceWindow: hwnd,
            Windowed: true.into(),
            PresentationInterval: 0x8000_0000, // D3DPRESENT_INTERVAL_IMMEDIATE
            ..Default::default()
        };
        let mut device_opt: Option<IDirect3DDevice9> = None;
        d3d.CreateDevice(
            0,
            D3DDEVTYPE_HAL,
            hwnd,
            D3DCREATE_HARDWARE_VERTEXPROCESSING as u32,
            &mut params,
            &mut device_opt,
        )
        .expect("CreateDevice");
        let device = device_opt.expect("device");
        let vtbl = *(device.as_raw() as *const *mut core::ffi::c_void);
        let present_slot = std::ptr::read((vtbl as *const usize).add(17));
        println!(
            "game device vtbl: {vtbl:p}, Present slot = {:#x}",
            present_slot
        );

        println!("READY");
        let _ = std::io::stdout().flush();

        let start = Instant::now();
        let deadline = args.seconds.map(|s| start + Duration::from_secs(s));
        let cpu = Duration::from_secs_f64(args.cpu_ms / 1000.0);
        let mut msg = Default::default();
        loop {
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    break;
                }
            }
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // A tiny bit of "rendering": clear to a dark blue.
            let _ = device.Clear(
                0,
                std::ptr::null(),
                0x1, // D3DCLEAR_TARGET
                0x00101018,
                1.0,
                0,
            );
            if device
                .Present(
                    std::ptr::null(),
                    std::ptr::null(),
                    HWND::default(),
                    std::ptr::null(),
                )
                .is_err()
            {
                // Device lost etc. — recreate is out of scope for the stand.
            }
            if !cpu.is_zero() {
                Sleep(cpu.as_millis() as u32);
            }
        }
        drop(device);
        drop(d3d);
    }
}
