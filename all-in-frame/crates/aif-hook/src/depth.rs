//! Depth Buffer Extraction & Shadow Texture Copying for Hazard-Free Warp Sampling.

use std::sync::Mutex;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11ShaderResourceView,
    ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_R24_UNORM_X8_TYPELESS,
    DXGI_FORMAT_R32_FLOAT,
};

pub struct DepthCopyTarget {
    pub texture: ID3D11Texture2D,
    pub srv: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
}

pub struct DepthTracker {
    last_candidate_resource: Option<ID3D11Resource>,
    shadow_copy: Option<DepthCopyTarget>,
}

impl DepthTracker {
    pub const fn new() -> Self {
        Self {
            last_candidate_resource: None,
            shadow_copy: None,
        }
    }

    /// Registers an observed DepthStencilView resource from the game's render pipeline.
    pub fn register_depth_resource(&mut self, resource: ID3D11Resource) {
        self.last_candidate_resource = Some(resource);
    }

    #[inline]
    pub fn has_active_depth(&self) -> bool {
        self.last_candidate_resource.is_some()
    }

    /// Prepares a safe, non-conflicting ShaderResourceView of the current scene depth
    /// by copying the active depth buffer into an isolated shadow texture.
    pub fn snapshot_depth_srv(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        target_width: u32,
        target_height: u32,
    ) -> Option<ID3D11ShaderResourceView> {
        let candidate = self.last_candidate_resource.as_ref()?;

        let depth_tex: ID3D11Texture2D = candidate.cast().ok()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { depth_tex.GetDesc(&mut desc) };

        // Ensure resolution matches the target viewport
        if desc.Width != target_width || desc.Height != target_height {
            return None;
        }

        let srv_format = match desc.Format {
            DXGI_FORMAT_D32_FLOAT | DXGI_FORMAT_R32_FLOAT => DXGI_FORMAT_R32_FLOAT,
            DXGI_FORMAT_D24_UNORM_S8_UINT | DXGI_FORMAT_R24_UNORM_X8_TYPELESS => {
                DXGI_FORMAT_R24_UNORM_X8_TYPELESS
            }
            _ => DXGI_FORMAT_R32_FLOAT,
        };

        // Recreate or reuse matching shadow copy
        let need_recreate = match &self.shadow_copy {
            Some(curr) => {
                curr.width != desc.Width || curr.height != desc.Height || curr.format != srv_format
            }
            None => true,
        };

        if need_recreate {
            let copy_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: srv_format,
                SampleDesc: desc.SampleDesc,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut new_tex: Option<ID3D11Texture2D> = None;
            let res = unsafe { device.CreateTexture2D(&copy_desc, None, Some(&mut new_tex)) };
            if res.is_err() {
                return None;
            }
            let new_tex = new_tex?;
            let mut new_srv: Option<ID3D11ShaderResourceView> = None;
            let srv_res = unsafe {
                device.CreateShaderResourceView(&new_tex, None, Some(&mut new_srv))
            };
            if srv_res.is_err() {
                return None;
            }
            let new_srv = new_srv?;

            self.shadow_copy = Some(DepthCopyTarget {
                texture: new_tex,
                srv: new_srv,
                width: desc.Width,
                height: desc.Height,
                format: srv_format,
            });
        }

        // Perform hardware CopyResource from game's depth into our shadow copy
        let shadow = self.shadow_copy.as_ref()?;
        unsafe {
            context.CopyResource(&shadow.texture, &depth_tex);
        }

        Some(shadow.srv.clone())
    }
}

pub static GLOBAL_DEPTH_TRACKER: Mutex<DepthTracker> = Mutex::new(DepthTracker::new());
