//! Standalone DirectX 11 3D Test Application for All In Frame Verification.
//! Renders a 3D perspective scene with moving geometry, active depth buffer,
//! and a stationary 2D HUD crosshair, with a simulated 30 FPS lock.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::time::Instant;

static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static CAM_YAW: AtomicU32 = AtomicU32::new(0);
static CAM_PITCH: AtomicU32 = AtomicU32::new(0);

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, ID3D11Buffer, ID3D11DepthStencilView,
    ID3D11Device, ID3D11DeviceContext, ID3D11InputLayout, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER,
    D3D11_BIND_DEPTH_STENCIL, D3D11_BIND_VERTEX_BUFFER, D3D11_BUFFER_DESC, D3D11_CLEAR_DEPTH,
    D3D11_CLEAR_STENCIL, D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_FLAG,
    D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAP_WRITE_DISCARD,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R32G32B32_FLOAT,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PostQuitMessage,
    RegisterClassW, ShowWindow, CS_HREDRAW, CS_VREDRAW, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE,
    WM_DESTROY, WM_KEYDOWN, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

#[repr(C)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[repr(C, align(16))]
struct SceneConstants {
    mvp: [[f32; 4]; 4],
}

const HLSL_SRC: &str = r#"
cbuffer ConstantBuffer : register(b0)
{
    matrix MVP;
};

struct VS_INPUT
{
    float3 Pos : POSITION;
    float4 Col : COLOR;
};

struct PS_INPUT
{
    float4 Pos : SV_POSITION;
    float4 Col : COLOR;
};

PS_INPUT VS(VS_INPUT input)
{
    PS_INPUT output;
    output.Pos = mul(MVP, float4(input.Pos, 1.0f));
    output.Col = input.Col;
    return output;
}

float4 PS(PS_INPUT input) : SV_Target
{
    return input.Col;
}
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== All In Frame 3D Test Benchmark Demo ===");
    println!("Target render rate: 30 FPS (simulated heavy game workload).");
    println!("With All In Frame AMW active, mouse look will remain silky smooth at your monitor's full refresh rate!");

    let width = 1280u32;
    let height = 720u32;

    // 1. Create Window
    let class_name = w!("AllInFrameDemoClass");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
        ..Default::default()
    };
    let hwnd = unsafe {
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR::from_raw(class_name.as_ptr()),
            w!("All In Frame — 3D Benchmark Demo (30 FPS Lock)"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            width as i32,
            height as i32,
            None,
            None,
            None,
            None,
        )?
    };

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    // 2. Direct3D 11 Setup
    let sc_desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: width,
            Height: height,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        OutputWindow: hwnd,
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..Default::default()
    };

    let mut device: Option<ID3D11Device> = None;
    let mut swapchain: Option<IDXGISwapChain> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut feature_level = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&sc_desc),
            Some(&mut swapchain),
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )?;
    }

    let device = device.unwrap();
    let swapchain = swapchain.unwrap();
    let context = context.unwrap();

    // 3. Render Target & Depth Stencil Buffer
    let backbuffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0)? };
    let mut rtv: Option<ID3D11RenderTargetView> = None;
    unsafe {
        device.CreateRenderTargetView(&backbuffer, None, Some(&mut rtv))?;
    }
    let rtv = rtv.unwrap();

    let depth_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_D24_UNORM_S8_UINT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_DEPTH_STENCIL.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut depth_tex: Option<ID3D11Texture2D> = None;
    unsafe {
        device.CreateTexture2D(&depth_desc, None, Some(&mut depth_tex))?;
    }
    let depth_tex = depth_tex.unwrap();

    let mut dsv: Option<ID3D11DepthStencilView> = None;
    unsafe {
        device.CreateDepthStencilView(&depth_tex, None, Some(&mut dsv))?;
    }
    let dsv = dsv.unwrap();

    // 4. Shaders & Geometry (3D Cube)
    let (vs, ps, layout) = compile_shaders(&device)?;
    let vb = create_cube_mesh(&device)?;

    let cb_desc = D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of::<SceneConstants>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut constant_buffer: Option<ID3D11Buffer> = None;
    unsafe {
        device.CreateBuffer(&cb_desc, None, Some(&mut constant_buffer))?;
    }
    let constant_buffer = constant_buffer.unwrap();

    let viewport = D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: width as f32,
        Height: height as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };

    // 5. Main Render Loop with 30 FPS simulated lock
    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    let mut angle: f32 = 0.0;
    let _start_time = Instant::now();

    while msg.message != windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
        let frame_start = Instant::now();

        unsafe {
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = DispatchMessageW(&msg);
                continue;
            }

            // Clear color and depth
            context.ClearRenderTargetView(&rtv, &[0.05, 0.08, 0.12, 1.0]);
            context.ClearDepthStencilView(
                &dsv,
                (D3D11_CLEAR_DEPTH.0 | D3D11_CLEAR_STENCIL.0) as u32,
                1.0,
                0,
            );

            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), Some(&dsv));
            context.RSSetViewports(Some(&[viewport]));

            // Update rotating MVP
            angle += 0.02;
            let yaw = f32::from_bits(CAM_YAW.load(Ordering::Relaxed));
            let pitch = f32::from_bits(CAM_PITCH.load(Ordering::Relaxed));
            let mvp = compute_mvp(angle, width as f32 / height as f32, yaw, pitch);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(
                &constant_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                &SceneConstants { mvp },
                mapped.pData as *mut SceneConstants,
                1,
            );
            context.Unmap(&constant_buffer, 0);

            // Draw 3D scene
            context.IASetInputLayout(&layout);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let stride = std::mem::size_of::<Vertex>() as u32;
            let offset = 0u32;
            context.IASetVertexBuffers(
                0,
                1,
                Some(&Some(vb.clone())),
                Some(&stride),
                Some(&offset),
            );

            context.VSSetShader(&vs, None);
            context.VSSetConstantBuffers(0, Some(&[Some(constant_buffer.clone())]));
            context.PSSetShader(&ps, None);

            context.Draw(36, 0);

            // Present to display (intercepted by All In Frame)
            let _ = swapchain.Present(1, DXGI_PRESENT(0));
        }

        // Lock engine render loop to ~30 FPS (33.3 ms per frame)
        let elapsed = frame_start.elapsed();
        if elapsed < std::time::Duration::from_millis(33) {
            std::thread::sleep(std::time::Duration::from_millis(33) - elapsed);
        }
    }

    Ok(())
}

fn compute_mvp(angle: f32, aspect: f32, yaw: f32, pitch: f32) -> [[f32; 4]; 4] {
    let total_angle = angle + yaw;
    let (s, c) = total_angle.sin_cos();
    let fov_rad = 90.0f32.to_radians();
    let tan_half = (fov_rad * 0.5).tan();
    let near = 0.1f32;
    let far = 100.0f32;

    [
        [c / (aspect * tan_half), 0.0, s * (far / (far - near)), s],
        [0.0, 1.0 / tan_half, pitch.sin() * 0.5, 0.0],
        [-s / (aspect * tan_half), 0.0, c * (far / (far - near)), c],
        [0.0, 0.0, -(near * far) / (far - near) + 3.0, 3.0],
    ]
}

fn compile_shaders(
    device: &ID3D11Device,
) -> Result<(ID3D11VertexShader, ID3D11PixelShader, ID3D11InputLayout), Box<dyn std::error::Error>> {
    let mut vs_blob = None;
    let mut ps_blob = None;

    unsafe {
        D3DCompile(
            HLSL_SRC.as_ptr() as *const c_void,
            HLSL_SRC.len(),
            None,
            None,
            None,
            s!("VS"),
            s!("vs_5_0"),
            0,
            0,
            &mut vs_blob,
            None,
        )?;
        D3DCompile(
            HLSL_SRC.as_ptr() as *const c_void,
            HLSL_SRC.len(),
            None,
            None,
            None,
            s!("PS"),
            s!("ps_5_0"),
            0,
            0,
            &mut ps_blob,
            None,
        )?;
    }

    let vs_blob = vs_blob.unwrap();
    let ps_blob = ps_blob.unwrap();

    let mut vs = None;
    let mut ps = None;
    let mut layout = None;

    let elements = [
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("POSITION"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32B32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: 0,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("COLOR"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: 12,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
    ];

    unsafe {
        let code_ptr = vs_blob.GetBufferPointer();
        let code_size = vs_blob.GetBufferSize();
        device.CreateVertexShader(
            std::slice::from_raw_parts(code_ptr as *const u8, code_size),
            None,
            Some(&mut vs),
        )?;
        device.CreateInputLayout(
            &elements,
            std::slice::from_raw_parts(code_ptr as *const u8, code_size),
            Some(&mut layout),
        )?;

        let ps_code = ps_blob.GetBufferPointer();
        let ps_size = ps_blob.GetBufferSize();
        device.CreatePixelShader(
            std::slice::from_raw_parts(ps_code as *const u8, ps_size),
            None,
            Some(&mut ps),
        )?;
    }

    Ok((vs.unwrap(), ps.unwrap(), layout.unwrap()))
}

fn create_cube_mesh(
    device: &ID3D11Device,
) -> Result<ID3D11Buffer, Box<dyn std::error::Error>> {
    let vertices = [
        // Front face (red/orange)
        Vertex { pos: [-1.0, -1.0, -1.0], color: [1.0, 0.2, 0.2, 1.0] },
        Vertex { pos: [-1.0,  1.0, -1.0], color: [1.0, 0.5, 0.2, 1.0] },
        Vertex { pos: [ 1.0,  1.0, -1.0], color: [1.0, 0.8, 0.2, 1.0] },
        Vertex { pos: [-1.0, -1.0, -1.0], color: [1.0, 0.2, 0.2, 1.0] },
        Vertex { pos: [ 1.0,  1.0, -1.0], color: [1.0, 0.8, 0.2, 1.0] },
        Vertex { pos: [ 1.0, -1.0, -1.0], color: [1.0, 0.3, 0.2, 1.0] },

        // Back face (green)
        Vertex { pos: [-1.0, -1.0,  1.0], color: [0.2, 1.0, 0.2, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.2, 1.0, 0.8, 1.0] },
        Vertex { pos: [-1.0,  1.0,  1.0], color: [0.4, 1.0, 0.2, 1.0] },
        Vertex { pos: [-1.0, -1.0,  1.0], color: [0.2, 1.0, 0.2, 1.0] },
        Vertex { pos: [ 1.0, -1.0,  1.0], color: [0.2, 0.8, 0.2, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.2, 1.0, 0.8, 1.0] },

        // Top face (blue)
        Vertex { pos: [-1.0,  1.0, -1.0], color: [0.2, 0.4, 1.0, 1.0] },
        Vertex { pos: [-1.0,  1.0,  1.0], color: [0.2, 0.7, 1.0, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.4, 0.9, 1.0, 1.0] },
        Vertex { pos: [-1.0,  1.0, -1.0], color: [0.2, 0.4, 1.0, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.4, 0.9, 1.0, 1.0] },
        Vertex { pos: [ 1.0,  1.0, -1.0], color: [0.3, 0.5, 1.0, 1.0] },

        // Bottom face (yellow)
        Vertex { pos: [-1.0, -1.0, -1.0], color: [1.0, 1.0, 0.2, 1.0] },
        Vertex { pos: [ 1.0, -1.0,  1.0], color: [0.9, 0.9, 0.2, 1.0] },
        Vertex { pos: [-1.0, -1.0,  1.0], color: [0.8, 0.8, 0.2, 1.0] },
        Vertex { pos: [-1.0, -1.0, -1.0], color: [1.0, 1.0, 0.2, 1.0] },
        Vertex { pos: [ 1.0, -1.0, -1.0], color: [1.0, 0.9, 0.2, 1.0] },
        Vertex { pos: [ 1.0, -1.0,  1.0], color: [0.9, 0.9, 0.2, 1.0] },

        // Left face (magenta)
        Vertex { pos: [-1.0, -1.0,  1.0], color: [1.0, 0.2, 1.0, 1.0] },
        Vertex { pos: [-1.0,  1.0, -1.0], color: [0.8, 0.2, 0.9, 1.0] },
        Vertex { pos: [-1.0, -1.0, -1.0], color: [1.0, 0.2, 0.8, 1.0] },
        Vertex { pos: [-1.0, -1.0,  1.0], color: [1.0, 0.2, 1.0, 1.0] },
        Vertex { pos: [-1.0,  1.0,  1.0], color: [0.9, 0.3, 1.0, 1.0] },
        Vertex { pos: [-1.0,  1.0, -1.0], color: [0.8, 0.2, 0.9, 1.0] },

        // Right face (cyan)
        Vertex { pos: [ 1.0, -1.0, -1.0], color: [0.2, 1.0, 1.0, 1.0] },
        Vertex { pos: [ 1.0,  1.0, -1.0], color: [0.3, 0.9, 0.9, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.4, 1.0, 1.0, 1.0] },
        Vertex { pos: [ 1.0, -1.0, -1.0], color: [0.2, 1.0, 1.0, 1.0] },
        Vertex { pos: [ 1.0,  1.0,  1.0], color: [0.4, 1.0, 1.0, 1.0] },
        Vertex { pos: [ 1.0, -1.0,  1.0], color: [0.2, 0.8, 0.9, 1.0] },
    ];

    let vb_desc = D3D11_BUFFER_DESC {
        ByteWidth: (std::mem::size_of::<Vertex>() * vertices.len()) as u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let sub_data = D3D11_SUBRESOURCE_DATA {
        pSysMem: vertices.as_ptr() as *const c_void,
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };
    let mut vb = None;
    unsafe {
        device.CreateBuffer(&vb_desc, Some(&sub_data), Some(&mut vb))?;
    }

    Ok(vb.unwrap())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN => {
            MOUSE_DOWN.store(true, Ordering::Relaxed);
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            LAST_X.store(x, Ordering::Relaxed);
            LAST_Y.store(y, Ordering::Relaxed);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP => {
            MOUSE_DOWN.store(false, Ordering::Relaxed);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE => {
            if MOUSE_DOWN.load(Ordering::Relaxed) {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let dx = x - LAST_X.swap(x, Ordering::Relaxed);
                let dy = y - LAST_Y.swap(y, Ordering::Relaxed);

                let cur_yaw = f32::from_bits(CAM_YAW.load(Ordering::Relaxed));
                let cur_pitch = f32::from_bits(CAM_PITCH.load(Ordering::Relaxed));
                let new_yaw = cur_yaw + dx as f32 * 0.005;
                let new_pitch = (cur_pitch + dy as f32 * 0.005).clamp(-1.4, 1.4);
                CAM_YAW.store(new_yaw.to_bits(), Ordering::Relaxed);
                CAM_PITCH.store(new_pitch.to_bits(), Ordering::Relaxed);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == 0x1B {
                // Escape key
                PostQuitMessage(0);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
