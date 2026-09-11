//! DirectX 11 Compute Shader Warp Pipeline Execution.

use std::ffi::c_void;

use windows::core::s;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11ComputeShader, ID3D11Device, ID3D11DeviceContext,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
    D3D11_BUFFER_DESC, D3D11_BIND_CONSTANT_BUFFER, D3D11_CPU_ACCESS_WRITE,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DYNAMIC,
};

use aif_common::math::{deg_to_rad, Mat3};
use aif_common::WarpConfig;

pub const WARP_SHADER_SOURCE: &str = include_str!("../shaders/warp.hlsl");

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct WarpConstants {
    pub rotation_matrix: [[f32; 4]; 3], // 3x3 packed as 3 float4 rows for HLSL alignment
    pub screen_size: [f32; 2],
    pub tan_half_fov_x: f32,
    pub tan_half_fov_y: f32,
    pub near_plane: f32,
    pub far_plane: f32,
    pub is_reverse_z: u32,
    pub hud_mask_enabled: u32,
    pub hud_depth_threshold: f32,
    pub inpainting_strength: f32,
    pub debug_depth: u32,
    pub fps_multiplier: u32,
}

pub struct WarpPipeline {
    compute_shader: ID3D11ComputeShader,
    constant_buffer: ID3D11Buffer,
    sampler_state: ID3D11SamplerState,
}

impl WarpPipeline {
    /// Compiles the HLSL compute shader and creates required DirectX 11 resources.
    pub fn new(device: &ID3D11Device) -> Result<Self, String> {
        let mut shader_blob = None;
        let mut error_blob = None;

        let hr = unsafe {
            D3DCompile(
                WARP_SHADER_SOURCE.as_ptr() as *const c_void,
                WARP_SHADER_SOURCE.len(),
                None,
                None,
                None,
                s!("CSMain"),
                s!("cs_5_0"),
                0,
                0,
                &mut shader_blob,
                Some(&mut error_blob),
            )
        };

        if hr.is_err() {
            let err_msg = if let Some(blob) = error_blob {
                let ptr = unsafe { blob.GetBufferPointer() } as *const u8;
                let size = unsafe { blob.GetBufferSize() };
                let bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                format!("D3DCompile error: {hr:?}")
            };
            return Err(format!("Shader compilation failed: {err_msg}"));
        }

        let blob = shader_blob.ok_or("Shader blob is null")?;
        let code_ptr = unsafe { blob.GetBufferPointer() };
        let code_size = unsafe { blob.GetBufferSize() };

        let mut compute_shader: Option<ID3D11ComputeShader> = None;
        unsafe {
            device
                .CreateComputeShader(
                    std::slice::from_raw_parts(code_ptr as *const u8, code_size),
                    None,
                    Some(&mut compute_shader),
                )
                .map_err(|e| format!("CreateComputeShader failed: {e}"))?;
        }
        let compute_shader = compute_shader.ok_or("Compute shader is null")?;

        // 2. Create Constant Buffer
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<WarpConstants>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut constant_buffer: Option<ID3D11Buffer> = None;
        unsafe {
            device
                .CreateBuffer(&cb_desc, None, Some(&mut constant_buffer))
                .map_err(|e| format!("CreateBuffer(ConstantBuffer) failed: {e}"))?;
        }
        let constant_buffer = constant_buffer.ok_or("Constant buffer is null")?;

        // 3. Create Sampler State (Linear Clamp)
        let samp_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let mut sampler_state: Option<ID3D11SamplerState> = None;
        unsafe {
            device
                .CreateSamplerState(&samp_desc, Some(&mut sampler_state))
                .map_err(|e| format!("CreateSamplerState failed: {e}"))?;
        }
        let sampler_state = sampler_state.ok_or("Sampler state is null")?;

        Ok(Self {
            compute_shader,
            constant_buffer,
            sampler_state,
        })
    }

    /// Dispatches the Asynchronous Mouse Warp compute shader across the frame.
    pub unsafe fn execute(
        &self,
        context: &ID3D11DeviceContext,
        color_srv: &ID3D11ShaderResourceView,
        depth_srv: &ID3D11ShaderResourceView,
        output_uav: &ID3D11UnorderedAccessView,
        width: u32,
        height: u32,
        delta_yaw: f32,
        delta_pitch: f32,
        config: &WarpConfig,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("Zero dimensions".into());
        }

        // 1. Calculate camera parameters
        let aspect = width as f32 / height as f32;
        let fov_rad = deg_to_rad(config.fov_degrees);
        let tan_half_fov_x = (fov_rad * 0.5).tan();
        let tan_half_fov_y = tan_half_fov_x / aspect.max(0.001);

        let rot = Mat3::from_yaw_pitch(delta_yaw, delta_pitch);

        let constants = WarpConstants {
            rotation_matrix: [
                [rot.m[0][0], rot.m[0][1], rot.m[0][2], 0.0],
                [rot.m[1][0], rot.m[1][1], rot.m[1][2], 0.0],
                [rot.m[2][0], rot.m[2][1], rot.m[2][2], 0.0],
            ],
            screen_size: [width as f32, height as f32],
            tan_half_fov_x,
            tan_half_fov_y,
            near_plane: config.depth_near,
            far_plane: config.depth_far,
            is_reverse_z: if config.is_reverse_z { 1 } else { 0 },
            hud_mask_enabled: if config.hud_mask_enabled { 1 } else { 0 },
            hud_depth_threshold: config.hud_depth_threshold,
            inpainting_strength: config.inpainting_strength,
            debug_depth: if config.debug_depth { 1 } else { 0 },
            fps_multiplier: config.fps_multiplier,
        };

        // 2. Map and update constant buffer
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(
                &self.constant_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )
            .map_err(|e| format!("Map(ConstantBuffer) failed: {e}"))?;

        std::ptr::copy_nonoverlapping(
            &constants as *const _ as *const c_void,
            mapped.pData,
            std::mem::size_of::<WarpConstants>(),
        );
        context.Unmap(&self.constant_buffer, 0);

        // 3. Bind shader, buffers, and views
        context.CSSetShader(&self.compute_shader, None);
        context.CSSetConstantBuffers(0, Some(&[Some(self.constant_buffer.clone())]));
        context.CSSetSamplers(0, Some(&[Some(self.sampler_state.clone())]));

        let srvs = [Some(color_srv.clone()), Some(depth_srv.clone())];
        context.CSSetShaderResources(0, Some(&srvs));

        let uavs = [Some(output_uav.clone())];
        context.CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);

        // 4. Dispatch compute kernel
        let groups_x = (width + 15) / 16;
        let groups_y = (height + 15) / 16;
        context.Dispatch(groups_x, groups_y, 1);

        // 5. Unbind resources to avoid DXGI hazard warnings on Present
        let null_srvs: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        context.CSSetShaderResources(0, Some(&null_srvs));

        let null_uavs: [Option<ID3D11UnorderedAccessView>; 1] = [None];
        context.CSSetUnorderedAccessViews(0, 1, Some(null_uavs.as_ptr()), None);

        Ok(())
    }
}
