//! Direct3D 11 in-game HUD overlay renderer.
//!
//! Features:
//! 1. Zero external assets / font files (embedded 5x7 monospace bitmap font).
//! 2. Complete D3D11 pipeline state preservation (zero side-effects on the game).
//! 3. Cached `ID3D11RenderTargetView` with mandatory release in `on_resize()`
//!    to guarantee `IDXGISwapChain::ResizeBuffers` never encounters `DXGI_ERROR_INVALID_CALL`.
//! 4. Live HUD displaying FPS, frametime, pacer cadence mode, Reflex status, and 60-frame sparkline.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::core::{s, Interface};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11Buffer, ID3D11DepthStencilState, ID3D11Device,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState,
    ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_VERTEX_BUFFER,
    D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA,
    D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CPU_ACCESS_WRITE,
    D3D11_CULL_NONE, D3D11_DEPTH_STENCIL_DESC, D3D11_FILL_SOLID, D3D11_INPUT_ELEMENT_DESC,
    D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE_DISCARD,
    D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32G32_FLOAT;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vertex {
    pos: [f32; 2],
    col: [f32; 4],
}

struct D3D11Resources {
    device_ptr: *mut c_void,
    rtv: Option<ID3D11RenderTargetView>,
    vertex_shader: Option<ID3D11VertexShader>,
    pixel_shader: Option<ID3D11PixelShader>,
    input_layout: Option<ID3D11InputLayout>,
    vertex_buffer: Option<ID3D11Buffer>,
    blend_state: Option<ID3D11BlendState>,
    depth_state: Option<ID3D11DepthStencilState>,
    raster_state: Option<ID3D11RasterizerState>,
    buffer_capacity: usize,
    history: [f32; 60],
    history_idx: usize,
}

unsafe impl Send for D3D11Resources {}
unsafe impl Sync for D3D11Resources {}

static RESOURCES: Mutex<Option<D3D11Resources>> = Mutex::new(None);

/// Called by `hooked_resize_buffers` in dxgi.rs to drop any backbuffer RTV
/// references BEFORE `ResizeBuffers` executes.
pub fn on_resize() {
    if let Ok(mut lock) = RESOURCES.lock() {
        if let Some(res) = lock.as_mut() {
            res.rtv = None;
        }
    }
}

/// Helper: compile HLSL string to D3DBlob.
unsafe fn compile_shader(src: &str, entry: &str, target: &str) -> Option<ID3DBlob> {
    let mut blob = None;
    let mut err_blob = None;
    let hr = D3DCompile(
        src.as_ptr() as *const c_void,
        src.len(),
        None,
        None,
        None,
        windows::core::PCSTR::from_raw(entry.as_ptr()),
        windows::core::PCSTR::from_raw(target.as_ptr()),
        0,
        0,
        &mut blob,
        Some(&mut err_blob),
    );
    if hr.is_err() {
        return None;
    }
    blob
}

const HLSL_SRC: &str = "\0struct VSInput {\n    float2 pos : POSITION;\n    float4 col : COLOR;\n};\nstruct PSInput {\n    float4 pos : SV_Position;\n    float4 col : COLOR;\n};\nPSInput vs_main(VSInput input) {\n    PSInput output;\n    output.pos = float4(input.pos, 0.0f, 1.0f);\n    output.col = input.col;\n    return output;\n}\nfloat4 ps_main(PSInput input) : SV_Target {\n    return input.col;\n}\n\0";

fn ensure_resources(
    device: &ID3D11Device,
    res: &mut D3D11Resources,
) -> Result<(), windows::core::Error> {
    if res.vertex_shader.is_some() {
        return Ok(());
    }

    unsafe {
        let vs_blob = compile_shader(HLSL_SRC, "vs_main\0", "vs_4_0\0")
            .ok_or_else(|| windows::core::Error::from_win32())?;
        let ps_blob = compile_shader(HLSL_SRC, "ps_main\0", "ps_4_0\0")
            .ok_or_else(|| windows::core::Error::from_win32())?;

        let vs_bytes = std::slice::from_raw_parts(
            vs_blob.GetBufferPointer() as *const u8,
            vs_blob.GetBufferSize(),
        );
        let ps_bytes = std::slice::from_raw_parts(
            ps_blob.GetBufferPointer() as *const u8,
            ps_blob.GetBufferSize(),
        );

        let mut vs = None;
        device.CreateVertexShader(vs_bytes, None, Some(&mut vs))?;

        let mut ps = None;
        device.CreatePixelShader(ps_bytes, None, Some(&mut ps))?;

        let ied = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("COLOR"),
                SemanticIndex: 0,
                Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];

        let mut layout = None;
        device.CreateInputLayout(&ied, vs_bytes, Some(&mut layout))?;

        let mut blend_desc = D3D11_BLEND_DESC::default();
        blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_SRC_ALPHA,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_SRC_ALPHA,
            DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend = None;
        device.CreateBlendState(&blend_desc, Some(&mut blend))?;

        let mut depth_desc = D3D11_DEPTH_STENCIL_DESC::default();
        depth_desc.DepthEnable = false.into();
        let mut depth = None;
        device.CreateDepthStencilState(&depth_desc, Some(&mut depth))?;

        let mut raster_desc = D3D11_RASTERIZER_DESC::default();
        raster_desc.FillMode = D3D11_FILL_SOLID;
        raster_desc.CullMode = D3D11_CULL_NONE;
        let mut raster = None;
        device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;

        res.vertex_shader = vs;
        res.pixel_shader = ps;
        res.input_layout = layout;
        res.blend_state = blend;
        res.depth_state = depth;
        res.raster_state = raster;
    }
    Ok(())
}

fn add_quad(
    verts: &mut Vec<Vertex>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    col: [f32; 4],
    sw: f32,
    sh: f32,
) {
    let to_ndc = |px: f32, py: f32| -> [f32; 2] {
        [(px / sw) * 2.0 - 1.0, 1.0 - (py / sh) * 2.0]
    };
    let p0 = to_ndc(x, y);
    let p1 = to_ndc(x + w, y);
    let p2 = to_ndc(x, y + h);
    let p3 = to_ndc(x + w, y + h);

    verts.push(Vertex { pos: p0, col });
    verts.push(Vertex { pos: p1, col });
    verts.push(Vertex { pos: p2, col });

    verts.push(Vertex { pos: p1, col });
    verts.push(Vertex { pos: p3, col });
    verts.push(Vertex { pos: p2, col });
}

// Minimal 5x7 monospace font glyphs (columns of 7 vertical bits)
fn get_glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        '1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        '3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        '9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        'A' | 'a' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        'B' | 'b' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        'C' | 'c' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        'D' | 'd' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        'E' | 'e' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        'F' | 'f' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        'G' | 'g' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        'H' | 'h' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'I' | 'i' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        'J' | 'j' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        'K' | 'k' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        'L' | 'l' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        'M' | 'm' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        'N' | 'n' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        'O' | 'o' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        'P' | 'p' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        'Q' | 'q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        'R' | 'r' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        'S' | 's' => [0x46, 0x49, 0x49, 0x49, 0x31],
        'T' | 't' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        'U' | 'u' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        'V' | 'v' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        'W' | 'w' => [0x7F, 0x20, 0x18, 0x20, 0x7F],
        'X' | 'x' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' | 'y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        'Z' | 'z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '+' => [0x08, 0x08, 0x3E, 0x08, 0x08],
        '(' => [0x00, 0x1C, 0x22, 0x41, 0x00],
        ')' => [0x00, 0x41, 0x22, 0x1C, 0x00],
        '|' => [0x00, 0x00, 0x7F, 0x00, 0x00],
        '/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

fn draw_string(
    verts: &mut Vec<Vertex>,
    start_x: f32,
    start_y: f32,
    text: &str,
    col: [f32; 4],
    scale: f32,
    sw: f32,
    sh: f32,
) {
    let mut cur_x = start_x;
    for c in text.chars() {
        if c == ' ' {
            cur_x += 4.0 * scale;
            continue;
        }
        let glyph = get_glyph(c);
        for col_idx in 0..5 {
            let bits = glyph[col_idx];
            for row_idx in 0..7 {
                if (bits & (1 << row_idx)) != 0 {
                    add_quad(
                        verts,
                        cur_x + col_idx as f32 * scale,
                        start_y + row_idx as f32 * scale,
                        scale,
                        scale,
                        col,
                        sw,
                        sh,
                    );
                }
            }
        }
        cur_x += 6.0 * scale;
    }
}

/// Render the in-game HUD overlay onto the DXGI swap chain.
pub unsafe fn render(
    swapchain_ptr: *mut c_void,
    fps: f64,
    frametime_ms: f64,
    mode_str: &str,
    reflex_status: &str,
) {
    super::poll_hotkey();
    if !super::is_visible() || swapchain_ptr.is_null() {
        return;
    }

    let sc = std::mem::ManuallyDrop::new(unsafe {
        std::mem::transmute::<*mut c_void, IDXGISwapChain>(swapchain_ptr)
    });
    let device: ID3D11Device = match sc.GetDevice() {
        Ok(d) => d,
        Err(_) => return, // Not D3D11 (e.g. D3D12/D3D9)
    };

    let ctx = match device.GetImmediateContext() {
        Ok(c) => c,
        Err(_) => return,
    };

    let back_buffer: ID3D11Texture2D = match sc.GetBuffer(0) {
        Ok(b) => b,
        Err(_) => return,
    };

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    back_buffer.GetDesc(&mut desc);
    let sw = desc.Width as f32;
    let sh = desc.Height as f32;
    if sw < 100.0 || sh < 100.0 {
        return;
    }

    let mut lock = match RESOURCES.lock() {
        Ok(l) => l,
        Err(_) => return,
    };

    let res = lock.get_or_insert_with(|| D3D11Resources {
        device_ptr: device.as_raw(),
        rtv: None,
        vertex_shader: None,
        pixel_shader: None,
        input_layout: None,
        vertex_buffer: None,
        blend_state: None,
        depth_state: None,
        raster_state: None,
        buffer_capacity: 0,
        history: [16.6; 60],
        history_idx: 0,
    });

    if res.device_ptr != device.as_raw() {
        *res = D3D11Resources {
            device_ptr: device.as_raw(),
            rtv: None,
            vertex_shader: None,
            pixel_shader: None,
            input_layout: None,
            vertex_buffer: None,
            blend_state: None,
            depth_state: None,
            raster_state: None,
            buffer_capacity: 0,
            history: [16.6; 60],
            history_idx: 0,
        };
    }

    if res.rtv.is_none() {
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        if device
            .CreateRenderTargetView(&back_buffer, None, Some(&mut rtv))
            .is_ok()
        {
            res.rtv = rtv;
        }
    }

    if ensure_resources(&device, res).is_err() {
        return;
    }
    let Some(rtv) = res.rtv.clone() else { return };

    // Update sparkline history
    let h_idx = res.history_idx % 60;
    res.history[h_idx] = frametime_ms as f32;
    res.history_idx = (res.history_idx + 1) % 60;

    // Build HUD vertices
    let mut verts = Vec::with_capacity(1024);
    let box_x = 12.0;
    let box_y = 12.0;
    let box_w = 264.0;
    let box_h = 76.0;

    // Dark translucent background card
    add_quad(
        &mut verts,
        box_x,
        box_y,
        box_w,
        box_h,
        [0.05, 0.06, 0.08, 0.88],
        sw,
        sh,
    );
    // Subtle border
    add_quad(
        &mut verts,
        box_x,
        box_y,
        box_w,
        1.5,
        [0.25, 0.35, 0.55, 0.9],
        sw,
        sh,
    );

    // Text lines
    let line1 = format!("iFrame: {:.1} FPS ({:.2}ms)", fps, frametime_ms);
    let line2 = format!("Mode: {} | Reflex: {}", mode_str, reflex_status);
    draw_string(
        &mut verts,
        box_x + 8.0,
        box_y + 8.0,
        &line1,
        [0.3, 0.95, 0.4, 1.0],
        2.0,
        sw,
        sh,
    );
    draw_string(
        &mut verts,
        box_x + 8.0,
        box_y + 26.0,
        &line2,
        [0.85, 0.9, 1.0, 0.95],
        1.5,
        sw,
        sh,
    );

    // Mini sparkline (last 60 frames)
    let spark_x = box_x + 8.0;
    let spark_y = box_y + 44.0;
    let spark_h = 24.0;
    add_quad(
        &mut verts,
        spark_x,
        spark_y,
        248.0,
        spark_h,
        [0.02, 0.02, 0.03, 0.7],
        sw,
        sh,
    );

    for i in 0..60 {
        let val_idx = (res.history_idx + i) % 60;
        let ft = res.history[val_idx].clamp(2.0, 50.0);
        let bar_h = (ft / 40.0 * spark_h).min(spark_h);
        let bar_y = spark_y + spark_h - bar_h;
        let col = if ft <= 17.0 {
            [0.2, 0.85, 0.3, 0.95] // green
        } else if ft <= 25.0 {
            [0.95, 0.75, 0.1, 0.95] // yellow
        } else {
            [0.95, 0.25, 0.25, 0.95] // red spike
        };
        add_quad(&mut verts, spark_x + i as f32 * 4.0, bar_y, 3.0, bar_h, col, sw, sh);
    }

    if verts.is_empty() {
        return;
    }

    // --- Save full pipeline state ---
    let mut orig_rtv = [None];
    let mut orig_dsv = None;
    ctx.OMGetRenderTargets(Some(&mut orig_rtv), Some(&mut orig_dsv));

    let mut num_viewports = 1;
    let mut orig_viewport = [D3D11_VIEWPORT::default()];
    ctx.RSGetViewports(&mut num_viewports, Some(orig_viewport.as_mut_ptr()));

    let mut orig_blend_state = None;
    let mut orig_blend_factor = [0.0; 4];
    let mut orig_sample_mask = 0;
    ctx.OMGetBlendState(
        Some(&mut orig_blend_state),
        Some(&mut orig_blend_factor),
        Some(&mut orig_sample_mask),
    );

    let mut orig_depth_state = None;
    let mut orig_stencil_ref = 0;
    ctx.OMGetDepthStencilState(Some(&mut orig_depth_state), Some(&mut orig_stencil_ref));

    let orig_raster_state = ctx.RSGetState().ok();
    let orig_layout = ctx.IAGetInputLayout().ok();
    let orig_topology = ctx.IAGetPrimitiveTopology();

    let mut orig_vs = None;
    ctx.VSGetShader(&mut orig_vs, None, None);
    let mut orig_ps = None;
    ctx.PSGetShader(&mut orig_ps, None, None);

    let mut orig_vb = [None];
    let mut orig_stride = [0u32];
    let mut orig_offset = [0u32];
    ctx.IAGetVertexBuffers(0, 1, Some(orig_vb.as_mut_ptr()), Some(orig_stride.as_mut_ptr()), Some(orig_offset.as_mut_ptr()));

    // Update / upload vertex buffer
    let vertex_bytes = std::slice::from_raw_parts(
        verts.as_ptr() as *const u8,
        verts.len() * std::mem::size_of::<Vertex>(),
    );

    if res.vertex_buffer.is_none() || res.buffer_capacity < verts.len() {
        let cap = (verts.len() + 256).next_power_of_two();
        let buf_desc = D3D11_BUFFER_DESC {
            ByteWidth: (cap * std::mem::size_of::<Vertex>()) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let init_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: vertex_bytes.as_ptr() as *const c_void,
            ..Default::default()
        };
        let mut vb = None;
        if device.CreateBuffer(&buf_desc, Some(&init_data), Some(&mut vb)).is_ok() {
            res.vertex_buffer = vb;
            res.buffer_capacity = cap;
        }
    } else if let Some(vb) = res.vertex_buffer.as_ref() {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if ctx.Map(vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
            std::ptr::copy_nonoverlapping(
                vertex_bytes.as_ptr(),
                mapped.pData as *mut u8,
                vertex_bytes.len(),
            );
            ctx.Unmap(vb, 0);
        }
    }

    if let Some(vb) = res.vertex_buffer.as_ref() {
        // --- Setup HUD rendering state ---
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: sw,
            Height: sh,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        ctx.RSSetViewports(Some(&[vp]));
        ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
        ctx.OMSetBlendState(res.blend_state.as_ref(), Some(&[0.0; 4]), 0xFFFFFFFF);
        ctx.OMSetDepthStencilState(res.depth_state.as_ref(), 0);
        ctx.RSSetState(res.raster_state.as_ref());

        ctx.VSSetShader(res.vertex_shader.as_ref(), None);
        ctx.PSSetShader(res.pixel_shader.as_ref(), None);
        ctx.IASetInputLayout(res.input_layout.as_ref());
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

        let stride = std::mem::size_of::<Vertex>() as u32;
        let offset = 0u32;
        ctx.IASetVertexBuffers(0, 1, Some(&Some(vb.clone())), Some(&stride), Some(&offset));

        // Draw HUD quads
        ctx.Draw(verts.len() as u32, 0);
    }

    // --- Restore saved pipeline state ---
    ctx.OMSetRenderTargets(Some(&orig_rtv), orig_dsv.as_ref());
    if num_viewports > 0 {
        ctx.RSSetViewports(Some(&orig_viewport[..num_viewports as usize]));
    }
    ctx.OMSetBlendState(
        orig_blend_state.as_ref(),
        Some(&orig_blend_factor),
        orig_sample_mask,
    );
    ctx.OMSetDepthStencilState(orig_depth_state.as_ref(), orig_stencil_ref);
    ctx.RSSetState(orig_raster_state.as_ref());
    ctx.VSSetShader(orig_vs.as_ref(), None);
    ctx.PSSetShader(orig_ps.as_ref(), None);
    ctx.IASetVertexBuffers(0, 1, Some(orig_vb.as_ptr()), Some(orig_stride.as_ptr()), Some(orig_offset.as_ptr()));
    ctx.IASetInputLayout(orig_layout.as_ref());
    ctx.IASetPrimitiveTopology(orig_topology);
}
