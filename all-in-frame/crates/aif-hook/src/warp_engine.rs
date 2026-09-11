//! High-Performance Runtime Warp Dispatcher & Texture Management.

use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11UnorderedAccessView, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS,
    D3D11_TEXTURE2D_DESC, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

use aif_common::input::delta_to_angles;
use aif_common::WarpConfig;
use aif_warp::WarpPipeline;

use crate::depth::GLOBAL_DEPTH_TRACKER;
use crate::raw_input::global_input;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;

pub struct RuntimeWarpContext {
    pipeline: Option<WarpPipeline>,
    color_copy: Option<ID3D11Texture2D>,
    color_srv: Option<ID3D11ShaderResourceView>,
    output_tex: Option<ID3D11Texture2D>,
    output_uav: Option<ID3D11UnorderedAccessView>,
    fallback_depth_srv: Option<ID3D11ShaderResourceView>,
    cached_depth_srv: Option<ID3D11ShaderResourceView>,
    accum_dx: f32,
    accum_dy: f32,
    width: u32,
    height: u32,
}

impl RuntimeWarpContext {
    pub const fn new() -> Self {
        Self {
            pipeline: None,
            color_copy: None,
            color_srv: None,
            output_tex: None,
            output_uav: None,
            fallback_depth_srv: None,
            cached_depth_srv: None,
            accum_dx: 0.0,
            accum_dy: 0.0,
            width: 0,
            height: 0,
        }
    }

    pub fn prepare_resources(
        &mut self,
        device: &ID3D11Device,
        width: u32,
        height: u32,
        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    ) -> Result<(), String> {
        if self.width == width && self.height == height && self.pipeline.is_some() {
            return Ok(());
        }

        // Initialize compute pipeline
        if self.pipeline.is_none() {
            self.pipeline = Some(WarpPipeline::new(device)?);
        }

        // 1. Create color copy texture (SRV bindable)
        let color_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut color_copy: Option<ID3D11Texture2D> = None;
        unsafe {
            device
                .CreateTexture2D(&color_desc, None, Some(&mut color_copy))
                .map_err(|e| format!("CreateTexture2D(color_copy) failed: {e}"))?;
        }
        let color_copy = color_copy.ok_or("color_copy is null")?;

        let mut color_srv: Option<ID3D11ShaderResourceView> = None;
        unsafe {
            device
                .CreateShaderResourceView(&color_copy, None, Some(&mut color_srv))
                .map_err(|e| format!("CreateShaderResourceView(color) failed: {e}"))?;
        }
        let color_srv = color_srv.ok_or("color_srv is null")?;

        // 2. Create output warped texture (UAV bindable)
        let uav_desc_tex = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut output_tex: Option<ID3D11Texture2D> = None;
        unsafe {
            device
                .CreateTexture2D(&uav_desc_tex, None, Some(&mut output_tex))
                .map_err(|e| format!("CreateTexture2D(output_tex) failed: {e}"))?;
        }
        let output_tex = output_tex.ok_or("output_tex is null")?;

        let uav_view_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_UAV { MipSlice: 0 },
            },
        };

        let mut output_uav: Option<ID3D11UnorderedAccessView> = None;
        unsafe {
            device
                .CreateUnorderedAccessView(&output_tex, Some(&uav_view_desc), Some(&mut output_uav))
                .map_err(|e| format!("CreateUnorderedAccessView failed: {e}"))?;
        }
        let output_uav = output_uav.ok_or("output_uav is null")?;

        if self.fallback_depth_srv.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: 1,
                Height: 1,
                MipLevels: 1,
                ArraySize: 1,
                Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_FLOAT,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let val = [0.5f32];
            let sub_data = D3D11_SUBRESOURCE_DATA {
                pSysMem: val.as_ptr() as *const std::ffi::c_void,
                SysMemPitch: 4,
                SysMemSlicePitch: 4,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            unsafe {
                device
                    .CreateTexture2D(&desc, Some(&sub_data), Some(&mut tex))
                    .map_err(|e| format!("CreateTexture2D(fallback_depth) failed: {e}"))?;
            }
            let tex = tex.unwrap();
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            unsafe {
                device
                    .CreateShaderResourceView(&tex, None, Some(&mut srv))
                    .map_err(|e| format!("CreateShaderResourceView(fallback_depth) failed: {e}"))?;
            }
            self.fallback_depth_srv = srv;
        }

        self.color_copy = Some(color_copy);
        self.color_srv = Some(color_srv);
        self.output_tex = Some(output_tex);
        self.output_uav = Some(output_uav);
        self.width = width;
        self.height = height;

        Ok(())
    }

    /// Executes the warp transformation on the active SwapChain backbuffer before Present.
    /// `is_extra_frame`: true when generating intermediate extrapolated frames (2x / 3x).
    pub fn process_frame(
        &mut self,
        swapchain: &IDXGISwapChain,
        config: &WarpConfig,
        is_extra_frame: bool,
    ) -> Result<(), String> {
        let device: ID3D11Device = unsafe { swapchain.GetDevice() }.map_err(|e| e.to_string())?;
        let context = unsafe { device.GetImmediateContext() }
            .map_err(|e| format!("GetImmediateContext failed: {e}"))?;

        let backbuffer: ID3D11Texture2D =
            unsafe { swapchain.GetBuffer(0) }.map_err(|e| e.to_string())?;
        let mut bb_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { backbuffer.GetDesc(&mut bb_desc) };

        self.prepare_resources(&device, bb_desc.Width, bb_desc.Height, bb_desc.Format)?;

        // 1. Snapshot or reuse pristine frame:
        if !is_extra_frame {
            // Fresh engine frame: capture pristine backbuffer before ANY warping
            let color_copy = self.color_copy.as_ref().unwrap();
            unsafe { context.CopyResource(color_copy, &backbuffer) };

            // Snapshot pristine depth buffer
            let depth_opt = {
                let mut tracker = GLOBAL_DEPTH_TRACKER.lock().unwrap();
                tracker.snapshot_depth_srv(&device, &context, bb_desc.Width, bb_desc.Height)
            };
            self.cached_depth_srv = depth_opt;

            // Reset cumulative delta for the new base frame
            self.accum_dx = 0.0;
            self.accum_dy = 0.0;
        }

        // 2. Obtain Depth Buffer SRV (cached pristine or fallback)
        let fallback = self.fallback_depth_srv.as_ref().unwrap();
        let depth_srv = self.cached_depth_srv.as_ref().unwrap_or(fallback);

        // 3. Accumulate instantaneous mouse delta
        let (raw_dx, raw_dy, _) = global_input().sample_and_reset();
        self.accum_dx += raw_dx as f32;
        self.accum_dy += raw_dy as f32;

        let mut dx = self.accum_dx;
        let dy = self.accum_dy;

        // Test Pulse check
        static LAST_PULSE: AtomicU32 = AtomicU32::new(0);
        if config.test_pulse != LAST_PULSE.load(Ordering::Relaxed) {
            LAST_PULSE.store(config.test_pulse, Ordering::Relaxed);
            dx += 600.0;
        }

        // Continuous Test Wave oscillation check
        if config.continuous_test_wave {
            static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
            dx += (t * 4.0).sin() * 250.0;
        }

        let (delta_yaw, delta_pitch) = if dx.abs() < 0.01 && dy.abs() < 0.01 {
            (0.0, 0.0)
        } else {
            delta_to_angles(dx as i32, dy as i32, config.yaw_sensitivity, config.pitch_sensitivity)
        };

        // 4. Dispatch Asynchronous Mouse Warp Compute Shader from pristine source
        let pipeline = self.pipeline.as_ref().unwrap();
        let color_srv = self.color_srv.as_ref().unwrap();
        let output_uav = self.output_uav.as_ref().unwrap();

        unsafe {
            pipeline.execute(
                &context,
                color_srv,
                depth_srv,
                output_uav,
                bb_desc.Width,
                bb_desc.Height,
                delta_yaw,
                delta_pitch,
                config,
            )?;
        }

        // 5. Blit warped output texture into current swapchain backbuffer
        let output_tex = self.output_tex.as_ref().unwrap();
        unsafe {
            context.CopyResource(&backbuffer, output_tex);
        }

        Ok(())
    }
}

pub static GLOBAL_WARP_CONTEXT: Mutex<RuntimeWarpContext> = Mutex::new(RuntimeWarpContext::new());
