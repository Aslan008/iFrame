//! Optical Flow & Frame Synthesizer Pipeline.
//! Real-time hardware-accelerated pyramidal motion estimation and bidirectional frame interpolation.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11ComputeShader, ID3D11Device, ID3D11DeviceContext,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11UnorderedAccessView, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_UNORDERED_ACCESS, D3D11_BUFFER_DESC, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_SAMPLER_DESC, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC,
    D3D11_CPU_ACCESS_WRITE, D3D11_MAP_WRITE_DISCARD,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R16G16_FLOAT, DXGI_SAMPLE_DESC,
};

const FLOW_SHADER_SRC: &str = include_str!("../shaders/flow.hlsl");
const SYNTH_SHADER_SRC: &str = include_str!("../shaders/synthesis.hlsl");

#[repr(C)]
struct FlowConstants {
    resolution: [f32; 2],
    motion_threshold: f32,
    hud_confidence: f32,
}

#[repr(C)]
struct SynthConstants {
    resolution: [f32; 2],
    alpha: f32,
    fps_multiplier: u32,
    debug_flow: u32,
    sharpening: f32,
    elapsed_seconds: f32,
    show_perf_hud: u32,
    game_fps: f32,
    output_fps: f32,
    frametime_ms: f32,
    pad: f32,
}

pub struct OpticalFlowPipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    flow_coarse_shader: ID3D11ComputeShader,
    flow_fine_shader: ID3D11ComputeShader,
    synth_shader: ID3D11ComputeShader,
    flow_cbuffer: ID3D11Buffer,
    synth_cbuffer: ID3D11Buffer,
    sampler: ID3D11SamplerState,

    // Managed intermediate textures
    coarse_tex: Option<ID3D11Texture2D>,
    coarse_srv: Option<ID3D11ShaderResourceView>,
    coarse_uav: Option<ID3D11UnorderedAccessView>,

    motion_tex: Option<ID3D11Texture2D>,
    motion_srv: Option<ID3D11ShaderResourceView>,
    motion_uav: Option<ID3D11UnorderedAccessView>,

    hud_tex: Option<ID3D11Texture2D>,
    hud_srv: Option<ID3D11ShaderResourceView>,
    hud_uav: Option<ID3D11UnorderedAccessView>,

    width: u32,
    height: u32,
}

impl OpticalFlowPipeline {
    pub fn new(device: ID3D11Device, context: ID3D11DeviceContext) -> Result<Self, String> {
        let flow_coarse_shader = compile_compute_shader(&device, FLOW_SHADER_SRC, "CSFlowCoarseMain")?;
        let flow_fine_shader = compile_compute_shader(&device, FLOW_SHADER_SRC, "CSFlowMain")?;
        let synth_shader = compile_compute_shader(&device, SYNTH_SHADER_SRC, "CSSynthMain")?;

        let flow_cbuffer = create_constant_buffer::<FlowConstants>(&device)?;
        let synth_cbuffer = create_constant_buffer::<SynthConstants>(&device)?;
        let sampler = create_linear_sampler(&device)?;

        Ok(Self {
            device,
            context,
            flow_coarse_shader,
            flow_fine_shader,
            synth_shader,
            flow_cbuffer,
            synth_cbuffer,
            sampler,
            coarse_tex: None,
            coarse_srv: None,
            coarse_uav: None,
            motion_tex: None,
            motion_srv: None,
            motion_uav: None,
            hud_tex: None,
            hud_srv: None,
            hud_uav: None,
            width: 0,
            height: 0,
        })
    }

    pub fn prepare_resources(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self.width == width && self.height == height && self.motion_tex.is_some() {
            return Ok(());
        }

        let half_w = (width / 2).max(1);
        let half_h = (height / 2).max(1);

        // Coarse Level-1 Motion Vectors texture (R16G16_FLOAT at half res)
        let (coarse_tex, coarse_srv, coarse_uav) = create_uav_texture(
            &self.device,
            half_w,
            half_h,
            DXGI_FORMAT_R16G16_FLOAT,
        )?;

        // Fine Level-0 Motion Vectors texture (R16G16_FLOAT at full res)
        let (motion_tex, motion_srv, motion_uav) = create_uav_texture(
            &self.device,
            width,
            height,
            DXGI_FORMAT_R16G16_FLOAT,
        )?;

        // Masks texture (R16G16_FLOAT: x = isHud, y = isOcc)
        let (hud_tex, hud_srv, hud_uav) = create_uav_texture(
            &self.device,
            width,
            height,
            DXGI_FORMAT_R16G16_FLOAT,
        )?;

        self.coarse_tex = Some(coarse_tex);
        self.coarse_srv = Some(coarse_srv);
        self.coarse_uav = Some(coarse_uav);

        self.motion_tex = Some(motion_tex);
        self.motion_srv = Some(motion_srv);
        self.motion_uav = Some(motion_uav);

        self.hud_tex = Some(hud_tex);
        self.hud_srv = Some(hud_srv);
        self.hud_uav = Some(hud_uav);

        self.width = width;
        self.height = height;

        Ok(())
    }

    /// Computes two-pass pyramidal optical flow between Frame N-1 and Frame N
    pub fn compute_flow(
        &mut self,
        prev_srv: &ID3D11ShaderResourceView,
        curr_srv: &ID3D11ShaderResourceView,
        motion_threshold: f32,
    ) -> Result<(), String> {
        let constants = FlowConstants {
            resolution: [self.width as f32, self.height as f32],
            motion_threshold,
            hud_confidence: 0.8,
        };

        unsafe {
            let mut mapped = windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.flow_cbuffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                .map_err(|e| format!("Map flow cbuffer: {e}"))?;
            std::ptr::copy_nonoverlapping(
                &constants as *const _ as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<FlowConstants>(),
            );
            self.context.Unmap(&self.flow_cbuffer, 0);

            self.context.CSSetConstantBuffers(0, Some(&[Some(self.flow_cbuffer.clone())]));
            self.context.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            // -------------------------------------------------------------
            // Pass 1: Coarse Pass (Half-Resolution, tracks large motions up to 64px)
            // -------------------------------------------------------------
            self.context.CSSetShader(&self.flow_coarse_shader, None);
            let srvs_coarse = [Some(prev_srv.clone()), Some(curr_srv.clone())];
            self.context.CSSetShaderResources(0, Some(&srvs_coarse));

            let uav_coarse = [self.coarse_uav.clone()];
            self.context.CSSetUnorderedAccessViews(0, 1, Some(uav_coarse.as_ptr()), None);

            let half_w = (self.width / 2).max(1);
            let half_h = (self.height / 2).max(1);
            self.context.Dispatch((half_w + 15) / 16, (half_h + 15) / 16, 1);

            // Unbind coarse UAV so it can be sampled as SRV in Pass 2
            let null_uav: [Option<ID3D11UnorderedAccessView>; 1] = [None];
            self.context.CSSetUnorderedAccessViews(0, 1, Some(null_uav.as_ptr()), None);

            // -------------------------------------------------------------
            // Pass 2: Fine Pass (Full-Resolution, warps coarse & refines sub-pixel)
            // -------------------------------------------------------------
            self.context.CSSetShader(&self.flow_fine_shader, None);
            let srvs_fine = [
                Some(prev_srv.clone()),
                Some(curr_srv.clone()),
                self.coarse_srv.clone(),
            ];
            self.context.CSSetShaderResources(0, Some(&srvs_fine));

            let uavs_fine = [self.motion_uav.clone(), self.hud_uav.clone()];
            self.context.CSSetUnorderedAccessViews(0, 2, Some(uavs_fine.as_ptr()), None);

            self.context.Dispatch((self.width + 15) / 16, (self.height + 15) / 16, 1);

            // Unbind all resources
            let null_uavs: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
            self.context.CSSetUnorderedAccessViews(0, 2, Some(null_uavs.as_ptr()), None);
            let null_srvs: [Option<ID3D11ShaderResourceView>; 3] = [None, None, None];
            self.context.CSSetShaderResources(0, Some(&null_srvs));
        }

        Ok(())
    }

    /// Synthesizes an intermediate frame at alpha in [0.0..1.0] with AMD FidelityFX CAS
    pub fn synthesize_frame(
        &mut self,
        prev_srv: &ID3D11ShaderResourceView,
        curr_srv: &ID3D11ShaderResourceView,
        output_uav: &ID3D11UnorderedAccessView,
        alpha: f32,
        fps_multiplier: u32,
        debug_flow: bool,
        sharpening: f32,
        elapsed_seconds: f32,
        show_perf_hud: bool,
        game_fps: f32,
        output_fps: f32,
        frametime_ms: f32,
    ) -> Result<(), String> {
        let constants = SynthConstants {
            resolution: [self.width as f32, self.height as f32],
            alpha,
            fps_multiplier,
            debug_flow: if debug_flow { 1 } else { 0 },
            sharpening: sharpening.clamp(0.0, 1.0),
            elapsed_seconds,
            show_perf_hud: if show_perf_hud { 1 } else { 0 },
            game_fps,
            output_fps,
            frametime_ms,
            pad: 0.0,
        };

        unsafe {
            let mut mapped = windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.synth_cbuffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                .map_err(|e| format!("Map synth cbuffer: {e}"))?;
            std::ptr::copy_nonoverlapping(
                &constants as *const _ as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<SynthConstants>(),
            );
            self.context.Unmap(&self.synth_cbuffer, 0);

            self.context.CSSetShader(&self.synth_shader, None);
            self.context.CSSetConstantBuffers(0, Some(&[Some(self.synth_cbuffer.clone())]));
            self.context.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            let srvs = [
                Some(prev_srv.clone()),
                Some(curr_srv.clone()),
                self.motion_srv.clone(),
                self.hud_srv.clone(),
            ];
            self.context.CSSetShaderResources(0, Some(&srvs));

            let uavs = [Some(output_uav.clone())];
            self.context.CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);

            let groups_x = (self.width + 15) / 16;
            let groups_y = (self.height + 15) / 16;
            self.context.Dispatch(groups_x, groups_y, 1);

            // Unbind resources
            let null_uav: [Option<ID3D11UnorderedAccessView>; 1] = [None];
            self.context.CSSetUnorderedAccessViews(0, 1, Some(null_uav.as_ptr()), None);
            let null_srvs: [Option<ID3D11ShaderResourceView>; 4] = [None, None, None, None];
            self.context.CSSetShaderResources(0, Some(&null_srvs));
        }

        Ok(())
    }
}

fn create_constant_buffer<T>(device: &ID3D11Device) -> Result<ID3D11Buffer, String> {
    let size = ((std::mem::size_of::<T>() + 15) / 16) * 16;
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer = None;
    unsafe {
        device
            .CreateBuffer(&desc, None, Some(&mut buffer))
            .map_err(|e| format!("CreateBuffer: {e}"))?;
    }
    Ok(buffer.unwrap())
}

fn create_linear_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState, String> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_NEVER,
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
        ..Default::default()
    };
    let mut sampler = None;
    unsafe {
        device
            .CreateSamplerState(&desc, Some(&mut sampler))
            .map_err(|e| format!("CreateSamplerState: {e}"))?;
    }
    Ok(sampler.unwrap())
}

fn create_uav_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView, ID3D11UnorderedAccessView), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut tex = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut tex))
            .map_err(|e| format!("CreateTexture2D: {e}"))?;
    }
    let tex = tex.unwrap();

    let mut srv = None;
    unsafe {
        let srv_desc = windows::Win32::Graphics::Direct3D11::D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D,
            Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                },
            },
        };
        device
            .CreateShaderResourceView(&tex, Some(&srv_desc), Some(&mut srv))
            .map_err(|e| format!("CreateShaderResourceView: {e}"))?;
    }
    let srv = srv.unwrap();

    let mut uav = None;
    unsafe {
        let uav_desc = windows::Win32::Graphics::Direct3D11::D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: windows::Win32::Graphics::Direct3D11::D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_UAV {
                    MipSlice: 0,
                },
            },
        };
        device
            .CreateUnorderedAccessView(&tex, Some(&uav_desc), Some(&mut uav))
            .map_err(|e| format!("CreateUnorderedAccessView: {e}"))?;
    }
    let uav = uav.unwrap();

    Ok((tex, srv, uav))
}

fn compile_compute_shader(
    device: &ID3D11Device,
    source: &str,
    entry_point: &str,
) -> Result<ID3D11ComputeShader, String> {
    let mut shader_blob = None;
    let mut error_blob = None;

    let source_bytes = source.as_bytes();
    let entry_point_cstr = std::ffi::CString::new(entry_point).map_err(|e| e.to_string())?;
    let target_cstr = std::ffi::CString::new("cs_5_0").map_err(|e| e.to_string())?;

    let hr = unsafe {
        D3DCompile(
            source_bytes.as_ptr() as *const _,
            source_bytes.len(),
            None,
            None,
            None,
            windows::core::PCSTR(entry_point_cstr.as_ptr() as *const u8),
            windows::core::PCSTR(target_cstr.as_ptr() as *const u8),
            0,
            0,
            &mut shader_blob,
            Some(&mut error_blob),
        )
    };

    if hr.is_err() {
        let err_msg = if let Some(blob) = error_blob {
            let slice = unsafe {
                std::slice::from_raw_parts(
                    blob.GetBufferPointer() as *const u8,
                    blob.GetBufferSize(),
                )
            };
            String::from_utf8_lossy(slice).to_string()
        } else {
            format!("{hr:?}")
        };
        return Err(format!("Shader compilation failed ({entry_point}): {err_msg}"));
    }

    let blob = shader_blob.unwrap();
    let mut cs = None;
    unsafe {
        let slice = std::slice::from_raw_parts(
            blob.GetBufferPointer() as *const u8,
            blob.GetBufferSize(),
        );
        device
            .CreateComputeShader(slice, None, Some(&mut cs))
            .map_err(|e| format!("CreateComputeShader: {e}"))?;
    }

    Ok(cs.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_optical_flow_shaders() {
        let mut device = None;
        let mut context = None;
        let feature_levels = [windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0];
        let hr = unsafe {
            windows::Win32::Graphics::Direct3D11::D3D11CreateDevice(
                None,
                windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_FLAG(0),
                Some(&feature_levels),
                windows::Win32::Graphics::Direct3D11::D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if hr.is_err() {
            eprintln!("Hardware D3D11 unavailable in test environment, skipping.");
            return;
        }
        let device = device.unwrap();
        let context = context.unwrap();

        let pipeline = OpticalFlowPipeline::new(device, context);
        assert!(pipeline.is_ok(), "OpticalFlowPipeline init: {:?}", pipeline.err());
    }
}
