//! Gamepad input polling via dynamic XInput loading with deadzone and auto-repeat.

use std::time::{Duration, Instant};
use windows::core::{s, w};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub const XINPUT_GAMEPAD_DPAD_UP: u16 = 0x0001;
pub const XINPUT_GAMEPAD_DPAD_DOWN: u16 = 0x0002;
pub const XINPUT_GAMEPAD_DPAD_LEFT: u16 = 0x0004;
pub const XINPUT_GAMEPAD_DPAD_RIGHT: u16 = 0x0008;
pub const XINPUT_GAMEPAD_START: u16 = 0x0010;
pub const XINPUT_GAMEPAD_BACK: u16 = 0x0020;
pub const XINPUT_GAMEPAD_LEFT_THUMB: u16 = 0x0040;
pub const XINPUT_GAMEPAD_RIGHT_THUMB: u16 = 0x0080;
pub const XINPUT_GAMEPAD_LEFT_SHOULDER: u16 = 0x0100;
pub const XINPUT_GAMEPAD_RIGHT_SHOULDER: u16 = 0x0200;
pub const XINPUT_GAMEPAD_A: u16 = 0x1000;
pub const XINPUT_GAMEPAD_B: u16 = 0x2000;
pub const XINPUT_GAMEPAD_X: u16 = 0x4000;
pub const XINPUT_GAMEPAD_Y: u16 = 0x8000;

pub const LEFT_THUMB_DEADZONE: i16 = 7849;
pub const LEFT_THUMB_FAST_THRESHOLD: i16 = 22000;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct XInputGamepad {
    pub w_buttons: u16,
    pub b_left_trigger: u8,
    pub b_right_trigger: u8,
    pub s_thumb_lx: i16,
    pub s_thumb_ly: i16,
    pub s_thumb_rx: i16,
    pub s_thumb_ry: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct XInputState {
    pub dw_packet_number: u32,
    pub gamepad: XInputGamepad,
}

type FnXInputGetState =
    unsafe extern "system" fn(dw_user_index: u32, p_state: *mut XInputState) -> u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadAction {
    FpsDown(i32),
    FpsUp(i32),
    NextPreset,
    PrevPreset,
    ToggleLimiter,
    ToggleAutoAttach,
    Attach,
    ToggleBigPicture,
}

pub struct GamepadTracker {
    _module: Option<HMODULE>,
    get_state_fn: Option<FnXInputGetState>,
    active_controller: Option<u32>,
    last_disconnect_scan: Instant,
    last_buttons: u16,
    last_packet: u32,
    /// Direction currently held for FPS stepping (-1 for down, +1 for up, 0 for none)
    held_dir: i32,
    hold_start: Option<Instant>,
    last_repeat: Instant,
    pub connected: bool,
}

impl Default for GamepadTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadTracker {
    pub fn new() -> Self {
        let (module, func) = Self::load_xinput();
        Self {
            _module: module,
            get_state_fn: func,
            active_controller: None,
            last_disconnect_scan: Instant::now() - Duration::from_secs(5),
            last_buttons: 0,
            last_packet: 0,
            held_dir: 0,
            hold_start: None,
            last_repeat: Instant::now(),
            connected: false,
        }
    }

    fn load_xinput() -> (Option<HMODULE>, Option<FnXInputGetState>) {
        let dll_names = [
            w!("xinput1_4.dll"),
            w!("xinput1_3.dll"),
            w!("xinput9_1_0.dll"),
        ];

        for name in dll_names {
            if let Ok(module) = unsafe { LoadLibraryW(name) } {
                if !module.is_invalid() {
                    if let Some(proc) = unsafe { GetProcAddress(module, s!("XInputGetState")) } {
                        let func: FnXInputGetState = unsafe { std::mem::transmute(proc) };
                        return (Some(module), Some(func));
                    }
                }
            }
        }
        (None, None)
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Poll connected controller and return triggered actions.
    pub fn poll(&mut self) -> Vec<GamepadAction> {
        let Some(get_state) = self.get_state_fn else {
            return Vec::new();
        };

        let now = Instant::now();
        let mut actions = Vec::new();

        // 1. Resolve controller index (throttle scan if disconnected to avoid 1-2ms delay per port)
        if self.active_controller.is_none() {
            if now.duration_since(self.last_disconnect_scan) < Duration::from_secs(2) {
                return actions;
            }
            self.last_disconnect_scan = now;

            for user_idx in 0..4 {
                let mut state = XInputState::default();
                let ret = unsafe { get_state(user_idx, &mut state) };
                if ret == 0 {
                    self.active_controller = Some(user_idx);
                    self.connected = true;
                    self.last_packet = state.dw_packet_number;
                    break;
                }
            }

            if self.active_controller.is_none() {
                self.connected = false;
                return actions;
            }
        }

        let user_idx = self.active_controller.unwrap();
        let mut state = XInputState::default();
        let ret = unsafe { get_state(user_idx, &mut state) };
        if ret != 0 {
            // Disconnected
            self.active_controller = None;
            self.connected = false;
            self.last_buttons = 0;
            self.held_dir = 0;
            self.hold_start = None;
            return actions;
        }

        self.connected = true;
        let pad = state.gamepad;
        let buttons = pad.w_buttons;
        let prev_buttons = self.last_buttons;

        // Button edge triggers (press event: 0 -> 1)
        let pressed = |mask: u16| -> bool { (buttons & mask != 0) && (prev_buttons & mask == 0) };

        if pressed(XINPUT_GAMEPAD_A) {
            actions.push(GamepadAction::Attach);
        }
        if pressed(XINPUT_GAMEPAD_Y) {
            actions.push(GamepadAction::ToggleLimiter);
        }
        if pressed(XINPUT_GAMEPAD_X) {
            actions.push(GamepadAction::ToggleAutoAttach);
        }
        if pressed(XINPUT_GAMEPAD_BACK) || pressed(XINPUT_GAMEPAD_LEFT_THUMB) {
            actions.push(GamepadAction::ToggleBigPicture);
        }
        if pressed(XINPUT_GAMEPAD_LEFT_SHOULDER) {
            actions.push(GamepadAction::PrevPreset);
        }
        if pressed(XINPUT_GAMEPAD_RIGHT_SHOULDER) {
            actions.push(GamepadAction::NextPreset);
        }

        // Stepping target FPS via D-Pad Left/Right or Left Stick X
        let stick_x = pad.s_thumb_lx;
        let mut current_dir = 0;
        let mut fast = false;

        if (buttons & XINPUT_GAMEPAD_DPAD_LEFT) != 0 || stick_x < -LEFT_THUMB_DEADZONE {
            current_dir = -1;
            if stick_x < -LEFT_THUMB_FAST_THRESHOLD {
                fast = true;
            }
        } else if (buttons & XINPUT_GAMEPAD_DPAD_RIGHT) != 0 || stick_x > LEFT_THUMB_DEADZONE {
            current_dir = 1;
            if stick_x > LEFT_THUMB_FAST_THRESHOLD {
                fast = true;
            }
        }

        let step_val = if fast { 5 } else { 1 };

        if current_dir != 0 {
            if self.held_dir != current_dir {
                // Initial press
                self.held_dir = current_dir;
                self.hold_start = Some(now);
                self.last_repeat = now;
                if current_dir > 0 {
                    actions.push(GamepadAction::FpsUp(step_val));
                } else {
                    actions.push(GamepadAction::FpsDown(step_val));
                }
            } else if let Some(start) = self.hold_start {
                // Check initial delay (300ms) then repeat interval (60ms)
                if now.duration_since(start) >= Duration::from_millis(300) {
                    if now.duration_since(self.last_repeat) >= Duration::from_millis(60) {
                        self.last_repeat = now;
                        if current_dir > 0 {
                            actions.push(GamepadAction::FpsUp(step_val));
                        } else {
                            actions.push(GamepadAction::FpsDown(step_val));
                        }
                    }
                }
            }
        } else {
            self.held_dir = 0;
            self.hold_start = None;
        }

        self.last_buttons = buttons;
        self.last_packet = state.dw_packet_number;
        actions
    }
}
