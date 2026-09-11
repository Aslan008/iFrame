//! All In Frame Warp Engine: HLSL Compute Shader Compilation & 3D Reprojection Pipeline.

pub mod pipeline;

pub use pipeline::{WarpConstants, WarpPipeline, WARP_SHADER_SOURCE};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use windows::core::s;
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;

    #[test]
    fn test_compile_warp_shader() {
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

        if let Err(e) = hr {
            let err_msg = if let Some(blob) = error_blob {
                let slice = unsafe {
                    std::slice::from_raw_parts(
                        blob.GetBufferPointer() as *const u8,
                        blob.GetBufferSize(),
                    )
                };
                String::from_utf8_lossy(slice).to_string()
            } else {
                e.to_string()
            };
            panic!("Shader compile failed: {err_msg}");
        }
        assert!(shader_blob.is_some());
    }
}
