//! 1000 Hz Raw Input Listener for Microsecond-Accurate Camera Rotation Tracking.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RAW_INPUT_DATA_COMMAND_FLAGS, RIDEV_INPUTSINK, RID_INPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, WINDOW_EX_STYLE, WM_DESTROY, WM_INPUT, WNDCLASSW, WS_OVERLAPPED,
};

use aif_common::input::InputAccumulator;

pub static GLOBAL_INPUT: OnceLock<InputAccumulator> = OnceLock::new();
static WORKER_RUNNING: AtomicBool = AtomicBool::new(false);

pub fn global_input() -> &'static InputAccumulator {
    GLOBAL_INPUT.get_or_init(InputAccumulator::new)
}

/// Starts the dedicated background thread that receives high-rate Raw Input.
pub fn start_raw_input_listener() {
    if WORKER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // Already running
    }

    std::thread::spawn(|| {
        let class_name = w!("AllInFrameRawInputClass");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(raw_input_wnd_proc),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        unsafe {
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR::from_raw(class_name.as_ptr()),
                w!("AllInFrameRawInputWindow"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            );

            if hwnd.is_err() {
                return;
            }
            let hwnd = hwnd.unwrap();

            // Register Raw Input for Mouse with RIDEV_INPUTSINK (receives input globally)
            let rid = RAWINPUTDEVICE {
                usUsagePage: 1, // Generic Desktop Controls
                usUsage: 2,     // Mouse
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };

            let _ = RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32);

            let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = DispatchMessageW(&msg);
            }
        }
    });
}

unsafe extern "system" fn raw_input_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            let hraw = HRAWINPUT(lparam.0 as *mut _);
            let mut size: u32 = 0;
            let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;

            let _ = GetRawInputData(
                hraw,
                RAW_INPUT_DATA_COMMAND_FLAGS(RID_INPUT.0),
                None,
                &mut size,
                header_size,
            );

            if size > 0 {
                let mut buffer = vec![0u8; size as usize];
                let read = GetRawInputData(
                    hraw,
                    RAW_INPUT_DATA_COMMAND_FLAGS(RID_INPUT.0),
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut size,
                    header_size,
                );

                if read != u32::MAX {
                    let raw = &*(buffer.as_ptr() as *const RAWINPUT);
                    if raw.header.dwType == 0 {
                        // 0 = RIM_TYPEMOUSE
                        let mouse = &raw.data.mouse;
                        let dx = mouse.lLastX;
                        let dy = mouse.lLastY;
                        if dx != 0 || dy != 0 {
                            global_input().push_delta(dx, dy);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
