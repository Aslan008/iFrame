//! Vblank phase sampling via DWM composition timing (`DwmGetCompositionTimingInfo`).
//!
//! Gives the JIT pacer the display tick grid: QPC of the last vblank and the
//! refresh rate. Valid for borderless/flip presentation — the mode iFrame
//! targets. `None` → the pacer degrades to VRR-style pacing automatically.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};

use iframe_common::pacer::VBlankHint;

/// Sample the display tick grid. Allocation-free; called once per Present.
pub fn hint() -> Option<VBlankHint> {
    let mut info = DWM_TIMING_INFO::default();
    info.cbSize = core::mem::size_of::<DWM_TIMING_INFO>() as u32;
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
    }
    if info.qpcVBlank == 0 {
        return None;
    }
    let num = info.rateRefresh.uiNumerator as f64;
    let den = info.rateRefresh.uiDenominator as f64;
    let refresh_hz = if num > 0.0 && den > 0.0 { num / den } else { 0.0 };
    if refresh_hz < 10.0 {
        return None; // implausible — let the pacer fall back
    }
    Some(VBlankHint {
        refresh_hz,
        last_vblank_qpc: info.qpcVBlank as i64,
    })
}