//! Zero-Copy VRAM Desktop & Target Window Capture via DXGI Desktop Duplication API.
//! High-performance multi-monitor GPU-to-GPU surface cropping without PCIe/CPU readback latency.

use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS,
    D3D11_BOX, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

pub struct CaptureFrame {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub timestamp: Instant,
}

pub struct DxgiCaptureEngine {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: Option<IDXGIOutputDuplication>,
    cached_frame: Option<ID3D11Texture2D>,
    cached_width: u32,
    cached_height: u32,
    screen_width: u32,
    screen_height: u32,
    current_monitor: Option<isize>,
    monitor_offset_x: i32,
    monitor_offset_y: i32,
}

impl DxgiCaptureEngine {
    pub fn new() -> Result<Self, String> {
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL_11_0;

        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice failed: {e}"))?;
        }

        let device = device.ok_or("D3D11 device is null")?;
        let context = context.ok_or("D3D11 context is null")?;

        let mut engine = Self {
            device,
            context,
            duplication: None,
            cached_frame: None,
            cached_width: 0,
            cached_height: 0,
            screen_width: 0,
            screen_height: 0,
            current_monitor: None,
            monitor_offset_x: 0,
            monitor_offset_y: 0,
        };

        engine.init_duplication_for_monitor(None)?;
        Ok(engine)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    fn init_duplication_for_monitor(&mut self, target_hmonitor: Option<isize>) -> Result<(), String> {
        self.duplication = None;

        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
            let adapter = factory
                .EnumAdapters1(0)
                .map_err(|e| format!("EnumAdapters1: {e}"))?;

            let mut target_output = None;
            let mut target_desc = None;

            let mut i = 0;
            while let Ok(out) = adapter.EnumOutputs(i) {
                if let Ok(desc) = out.GetDesc() {
                    if let Some(hmon) = target_hmonitor {
                        if desc.Monitor.0 as isize == hmon {
                            target_output = Some(out);
                            target_desc = Some(desc);
                            break;
                        }
                    } else if i == 0 {
                        target_output = Some(out);
                        target_desc = Some(desc);
                        break;
                    }
                }
                i += 1;
            }

            let output = match target_output {
                Some(o) => o,
                None => adapter.EnumOutputs(0).map_err(|e| format!("EnumOutputs(0): {e}"))?,
            };
            let desc = match target_desc {
                Some(d) => d,
                None => output.GetDesc().map_err(|e| format!("GetDesc: {e}"))?,
            };

            let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("cast to Output1: {e}"))?;

            self.screen_width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
            self.screen_height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;
            self.monitor_offset_x = desc.DesktopCoordinates.left;
            self.monitor_offset_y = desc.DesktopCoordinates.top;
            self.current_monitor = Some(desc.Monitor.0 as isize);

            let dupl = output1
                .DuplicateOutput(&self.device)
                .map_err(|e| format!("DuplicateOutput failed: {e}"))?;

            self.duplication = Some(dupl);
        }

        Ok(())
    }

    /// Captures the target window's rect, cropped in VRAM, without CPU readback.
    /// Dynamically switches between monitors if the window is moved across screens.
    pub fn acquire_frame(&mut self, target_hwnd: Option<HWND>, timeout_ms: u32) -> Result<Option<CaptureFrame>, String> {
        let hmonitor = target_hwnd.map(|hwnd| unsafe {
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST).0 as isize
        });

        if (hmonitor.is_some() && hmonitor != self.current_monitor) || self.duplication.is_none() {
            let _ = self.init_duplication_for_monitor(hmonitor);
        }

        let dupl = match &self.duplication {
            Some(d) => d,
            None => return Ok(None),
        };

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;

        let hr = unsafe {
            dupl.AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
        };

        if let Err(err) = hr {
            if err.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            if err.code() == DXGI_ERROR_ACCESS_LOST {
                let _ = self.init_duplication_for_monitor(hmonitor);
                return Ok(None);
            }
            return Err(format!("AcquireNextFrame failed: {err}"));
        }

        let resource = match resource {
            Some(r) => r,
            None => {
                let _ = unsafe { dupl.ReleaseFrame() };
                return Ok(None);
            }
        };

        let desktop_tex: ID3D11Texture2D = resource
            .cast()
            .map_err(|e| format!("cast resource to Texture2D: {e}"))?;

        // Determine destination dimensions and crop box relative to active monitor coordinates
        let (crop_box, dest_width, dest_height) = if let Some(hwnd) = target_hwnd {
            let mut client_rect = RECT::default();
            let mut top_left = POINT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut client_rect);
                let _ = ClientToScreen(hwnd, &mut top_left);
            }

            let rel_x = top_left.x - self.monitor_offset_x;
            let rel_y = top_left.y - self.monitor_offset_y;

            let left = rel_x.max(0) as u32;
            let top = rel_y.max(0) as u32;
            let w = (client_rect.right - client_rect.left).max(1) as u32;
            let h = (client_rect.bottom - client_rect.top).max(1) as u32;

            let right = (left + w).min(self.screen_width);
            let bottom = (top + h).min(self.screen_height);
            let w_actual = right.saturating_sub(left);
            let h_actual = bottom.saturating_sub(top);

            if w_actual == 0 || h_actual == 0 {
                let _ = unsafe { dupl.ReleaseFrame() };
                return Ok(None);
            }

            let box_rect = D3D11_BOX {
                left,
                top,
                front: 0,
                right,
                bottom,
                back: 1,
            };
            (Some(box_rect), w_actual, h_actual)
        } else {
            (None, self.screen_width, self.screen_height)
        };

        // Ensure cached destination texture matches dimensions
        if self.cached_width != dest_width || self.cached_height != dest_height || self.cached_frame.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: dest_width,
                Height: dest_height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut tex = None;
            unsafe {
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut tex))
                    .map_err(|e| format!("CreateTexture2D for crop cache failed: {e}"))?;
            }
            self.cached_frame = Some(tex.unwrap());
            self.cached_width = dest_width;
            self.cached_height = dest_height;
        }

        let dst_tex = self.cached_frame.as_ref().unwrap();

        // GPU-to-GPU sub-resource copy entirely in VRAM
        unsafe {
            if let Some(crop) = crop_box {
                self.context.CopySubresourceRegion(
                    dst_tex,
                    0,
                    0,
                    0,
                    0,
                    &desktop_tex,
                    0,
                    Some(&crop),
                );
            } else {
                self.context.CopyResource(dst_tex, &desktop_tex);
            }
            let _ = dupl.ReleaseFrame();
        }

        Ok(Some(CaptureFrame {
            texture: dst_tex.clone(),
            width: dest_width,
            height: dest_height,
            timestamp: Instant::now(),
        }))
    }
}
