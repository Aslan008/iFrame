//! Independent Decoupled Presenter & Frame Generation Engine.
//! Runs asynchronously to the target game, driving the monitor with optical-flow synthesized frames.

use aif_capture::DxgiCaptureEngine;
use aif_flow::OpticalFlowPipeline;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{w, Interface, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11UnorderedAccessView,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS, D3D11_TEXTURE2D_DESC,
    D3D11_TEX2D_SRV, D3D11_TEX2D_UAV, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIFactory2,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, PeekMessageW, RegisterClassW, SetWindowPos,
    ShowWindow, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, MSG, PM_REMOVE,
    SWP_NOACTIVATE, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP,
};

pub struct PresenterTelemetry {
    pub is_running: Arc<AtomicBool>,
    pub game_fps_bits: Arc<AtomicU32>,
    pub output_fps_bits: Arc<AtomicU32>,
    pub frames_captured: Arc<AtomicU64>,
    pub frames_presented: Arc<AtomicU64>,
}

impl PresenterTelemetry {
    pub fn new() -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            game_fps_bits: Arc::new(AtomicU32::new(0)),
            output_fps_bits: Arc::new(AtomicU32::new(0)),
            frames_captured: Arc::new(AtomicU64::new(0)),
            frames_presented: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn game_fps(&self) -> f32 {
        f32::from_bits(self.game_fps_bits.load(Ordering::Relaxed))
    }

    pub fn output_fps(&self) -> f32 {
        f32::from_bits(self.output_fps_bits.load(Ordering::Relaxed))
    }
}

pub struct PresenterController {
    stop_signal: Arc<AtomicBool>,
    telemetry: PresenterTelemetry,
    worker_handle: Option<std::thread::JoinHandle<()>>,
}

impl PresenterController {
    pub fn new() -> Self {
        Self {
            stop_signal: Arc::new(AtomicBool::new(false)),
            telemetry: PresenterTelemetry::new(),
            worker_handle: None,
        }
    }

    pub fn telemetry(&self) -> &PresenterTelemetry {
        &self.telemetry
    }

    pub fn is_running(&self) -> bool {
        self.telemetry.is_running.load(Ordering::Relaxed)
    }

    pub fn start(
        &mut self,
        target_hwnd: HWND,
        fps_multiplier: u32,
        debug_flow: bool,
        sharpening: f32,
    ) -> Result<(), String> {
        self.stop();

        // Audio notification chime (Asterisk)
        let _ = unsafe {
            windows::Win32::System::Diagnostics::Debug::MessageBeep(
                windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000040),
            )
        };

        self.stop_signal.store(false, Ordering::SeqCst);
        let stop_flag = self.stop_signal.clone();
        let is_running_flag = self.telemetry.is_running.clone();
        let game_fps_bits = self.telemetry.game_fps_bits.clone();
        let output_fps_bits = self.telemetry.output_fps_bits.clone();
        let frames_captured = self.telemetry.frames_captured.clone();
        let frames_presented = self.telemetry.frames_presented.clone();

        let target_hwnd_raw = target_hwnd.0 as usize;

        let handle = std::thread::Builder::new()
            .name("aif-presenter-thread".to_string())
            .spawn(move || {
                is_running_flag.store(true, Ordering::SeqCst);
                let hwnd = HWND(target_hwnd_raw as *mut _);
                let res = run_presenter_loop(
                    hwnd,
                    fps_multiplier,
                    debug_flow,
                    sharpening,
                    stop_flag,
                    game_fps_bits,
                    output_fps_bits,
                    frames_captured,
                    frames_presented,
                );
                if let Err(e) = res {
                    eprintln!("Presenter loop error: {e}");
                }
                is_running_flag.store(false, Ordering::SeqCst);
            })
            .map_err(|e| format!("Failed to spawn presenter thread: {e}"))?;

        self.worker_handle = Some(handle);
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.is_running() {
            // Audio notification chime (Low tone on stop)
            let _ = unsafe {
                windows::Win32::System::Diagnostics::Debug::MessageBeep(
                    windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000000),
                )
            };
        }
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
        self.telemetry.is_running.store(false, Ordering::SeqCst);
    }
}

impl Drop for PresenterController {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST => {
            // HTTRANSPARENT (-1): mouse events pass directly through to the underlying game!
            LRESULT(-1)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_overlay_window(
    width: u32,
    height: u32,
    x: i32,
    y: i32,
) -> Result<HWND, String> {
    unsafe {
        let class_name = w!("AifOverlayWindowClass");
        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| format!("{e}"))?;

        let wnd_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        let _ = RegisterClassW(&wnd_class);

        // Standard Topmost Popup window (no WS_EX_LAYERED, so Direct3D11 swapchain renders at 100% full brightness)
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0x08000000),
            class_name,
            w!("All In Frame Frame Generator Overlay"),
            WS_POPUP,
            x,
            y,
            width as i32,
            height as i32,
            None,
            None,
            Some(hinstance.into()),
            None,
        ).map_err(|e| format!("CreateWindowExW failed: {e}"))?;

        // Exclude overlay from desktop capture so Desktop Duplication sees directly through it to the game!
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::WDA_EXCLUDEFROMCAPTURE,
        );

        let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

fn run_presenter_loop(
    target_hwnd: HWND,
    fps_multiplier: u32,
    debug_flow: bool,
    sharpening: f32,
    stop_flag: Arc<AtomicBool>,
    game_fps_bits: Arc<AtomicU32>,
    output_fps_bits: Arc<AtomicU32>,
    frames_captured: Arc<AtomicU64>,
    frames_presented: Arc<AtomicU64>,
) -> Result<(), String> {
    // 1. Set Windows timer resolution to 1ms for sub-millisecond frame pacing
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }

    let mut capture = DxgiCaptureEngine::new()?;
    let device = capture.device().clone();
    let context = capture.context().clone();

    let mut flow_pipeline = OpticalFlowPipeline::new(device.clone(), context.clone())?;

    // Determine initial target rect
    let mut client_rect = RECT::default();
    let mut top_left = POINT::default();
    unsafe {
        let _ = GetClientRect(target_hwnd, &mut client_rect);
        let _ = ClientToScreen(target_hwnd, &mut top_left);
    }
    let mut width = (client_rect.right - client_rect.left).max(100) as u32;
    let mut height = (client_rect.bottom - client_rect.top).max(100) as u32;

    let overlay_hwnd = create_overlay_window(width, height, top_left.x, top_left.y)?;

    // Create D3D11 SwapChain for Overlay with VRR Tear-Free support
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("{e}"))?;
    let factory2: IDXGIFactory2 = factory.cast().map_err(|e| format!("{e}"))?;

    let sc_desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: BOOL::from(false),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: windows::Win32::Graphics::Dxgi::DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: windows::Win32::Graphics::Dxgi::Common::DXGI_ALPHA_MODE_UNSPECIFIED,
        Flags: windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32,
    };

    let (swapchain, allow_tearing) = match unsafe {
        factory2.CreateSwapChainForHwnd(&device, overlay_hwnd, &sc_desc, None, None)
    } {
        Ok(sc) => (sc, true),
        Err(_) => {
            let mut fallback_desc = sc_desc;
            fallback_desc.Flags = 0;
            let sc = unsafe {
                factory2.CreateSwapChainForHwnd(&device, overlay_hwnd, &fallback_desc, None, None)
            }.map_err(|e| format!("CreateSwapChainForHwnd failed: {e}"))?;
            (sc, false)
        }
    };

    flow_pipeline.prepare_resources(width, height)?;

    // Intermediate frame textures
    let (mut prev_tex, mut prev_srv) = create_cached_texture(&device, width, height)?;
    let (mut curr_tex, mut curr_srv) = create_cached_texture(&device, width, height)?;
    let (mut synth_tex, mut synth_uav) = create_uav_surface(&device, width, height)?;

    let start_time = Instant::now();
    let mut last_capture_time = Instant::now();
    let mut last_present_time = Instant::now();
    let mut smooth_game_fps = 0.0f32;
    let mut smooth_out_fps = 0.0f32;
    let mut has_first_frame = false;
    let mut show_perf_hud = false;
    let mut f12_prev = false;

    while !stop_flag.load(Ordering::Relaxed) {
        // Toggle in-game Performance HUD via F12
        let f12_down = unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_F12.0 as i32,
            )
        } < 0;
        if f12_down && !f12_prev {
            show_perf_hud = !show_perf_hud;
        }
        f12_prev = f12_down;

        // Pump overlay window messages
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(overlay_hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = DispatchMessageW(&msg);
            }
        }

        // Align overlay position to target window
        let mut cur_rect = RECT::default();
        let mut cur_pos = POINT::default();
        unsafe {
            let _ = GetClientRect(target_hwnd, &mut cur_rect);
            let _ = ClientToScreen(target_hwnd, &mut cur_pos);
            let _ = SetWindowPos(
                overlay_hwnd,
                Some(HWND_TOPMOST),
                cur_pos.x,
                cur_pos.y,
                cur_rect.right - cur_rect.left,
                cur_rect.bottom - cur_rect.top,
                SWP_NOACTIVATE,
            );
        }

        // 1. Acquire new frame from game window via zero-copy VRAM capture
        if let Ok(Some(frame)) = capture.acquire_frame(Some(target_hwnd), 10) {
            // Recreate textures if game resolution changed or differs
            if frame.width != width || frame.height != height {
                width = frame.width;
                height = frame.height;
                if let Ok((p_tex, p_srv)) = create_cached_texture(&device, width, height) {
                    prev_tex = p_tex;
                    prev_srv = p_srv;
                }
                if let Ok((c_tex, c_srv)) = create_cached_texture(&device, width, height) {
                    curr_tex = c_tex;
                    curr_srv = c_srv;
                }
                if let Ok((s_tex, s_uav)) = create_uav_surface(&device, width, height) {
                    synth_tex = s_tex;
                    synth_uav = s_uav;
                }
                let resize_flags = if allow_tearing {
                    windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
                } else {
                    windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG(0)
                };
                let _ = unsafe { swapchain.ResizeBuffers(2, width, height, DXGI_FORMAT_B8G8R8A8_UNORM, resize_flags) };
                let _ = flow_pipeline.prepare_resources(width, height);
            }

            let now = Instant::now();
            let dt = now.duration_since(last_capture_time).as_secs_f32().max(0.001);
            last_capture_time = now;
            let instant_fps = 1.0 / dt;
            smooth_game_fps = if smooth_game_fps <= 0.0 { instant_fps } else { smooth_game_fps * 0.9 + instant_fps * 0.1 };
            game_fps_bits.store(smooth_game_fps.to_bits(), Ordering::Relaxed);
            frames_captured.fetch_add(1, Ordering::Relaxed);

            // Shift history: prev <- curr, curr <- captured
            unsafe {
                context.CopyResource(&prev_tex, &curr_tex);
                context.CopyResource(&curr_tex, &frame.texture);
            }

            if !has_first_frame {
                has_first_frame = true;
                continue;
            }

            // Compute hierarchical optical flow between previous and current frame
            let _ = flow_pipeline.compute_flow(&prev_srv, &curr_srv, 1.2);

            // 2. Multi-frame generation loop (Decoupled Presentation)
            let mult = fps_multiplier.max(1);
            let steps = mult;
            let elapsed_sec = start_time.elapsed().as_secs_f32();

            for step in 1..=steps {
                let alpha = (step as f32) / (mult as f32);

                let backbuffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0) }
                    .map_err(|e| format!("{e}"))?;

                if step == steps {
                    // Final primary step: blit current pristine frame
                    unsafe { context.CopyResource(&backbuffer, &curr_tex) };
                } else {
                    // Intermediate synthesized step: synthesize bidirectional frame with FidelityFX CAS and live HUD
                    let frametime_ms = if smooth_out_fps > 0.0 { 1000.0 / smooth_out_fps } else { 16.6 };
                    let _ = flow_pipeline.synthesize_frame(
                        &prev_srv,
                        &curr_srv,
                        &synth_uav,
                        alpha,
                        mult,
                        debug_flow,
                        sharpening,
                        elapsed_sec,
                        show_perf_hud,
                        smooth_game_fps,
                        smooth_out_fps,
                        frametime_ms,
                    );
                    unsafe { context.CopyResource(&backbuffer, &synth_tex) };
                }

                // Present intermediate frame (with VRR tearing if enabled)
                let present_flags = if allow_tearing {
                    windows::Win32::Graphics::Dxgi::DXGI_PRESENT_ALLOW_TEARING
                } else {
                    windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)
                };
                let _ = unsafe { swapchain.Present(0, present_flags) };
                frames_presented.fetch_add(1, Ordering::Relaxed);

                let now_out = Instant::now();
                let dt_out = now_out.duration_since(last_present_time).as_secs_f32().max(0.001);
                last_present_time = now_out;
                let instant_out_fps = 1.0 / dt_out;
                smooth_out_fps = if smooth_out_fps <= 0.0 { instant_out_fps } else { smooth_out_fps * 0.95 + instant_out_fps * 0.05 };
                output_fps_bits.store(smooth_out_fps.to_bits(), Ordering::Relaxed);

                // Precise pacing sleep slice between synthesized frames
                if mult > 1 && step < steps {
                    let slice = Duration::from_secs_f32((dt / (mult as f32)).clamp(0.002, 0.033));
                    std::thread::sleep(slice);
                }
            }
        } else {
            // No new frame from game yet: sleep briefly to yield CPU
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    unsafe {
        windows::Win32::Media::timeEndPeriod(1);
        let _ = DestroyWindow(overlay_hwnd);
    }

    Ok(())
}

fn create_cached_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let tex = unsafe {
        let mut t = None;
        device.CreateTexture2D(&desc, None, Some(&mut t))
            .map_err(|e| format!("CreateTexture2D: {e}"))?;
        t.unwrap()
    };

    let srv_desc = windows::Win32::Graphics::Direct3D11::D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        ViewDimension: windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
        },
    };

    let srv = unsafe {
        let mut s = None;
        device.CreateShaderResourceView(&tex, Some(&srv_desc), Some(&mut s))
            .map_err(|e| format!("CreateShaderResourceView: {e}"))?;
        s.unwrap()
    };

    Ok((tex, srv))
}

fn create_uav_surface(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, ID3D11UnorderedAccessView), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let tex = unsafe {
        let mut t = None;
        device.CreateTexture2D(&desc, None, Some(&mut t))
            .map_err(|e| format!("CreateTexture2D: {e}"))?;
        t.unwrap()
    };

    let uav_desc = windows::Win32::Graphics::Direct3D11::D3D11_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        ViewDimension: windows::Win32::Graphics::Direct3D11::D3D11_UAV_DIMENSION_TEXTURE2D,
        Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
        },
    };

    let uav = unsafe {
        let mut u = None;
        device.CreateUnorderedAccessView(&tex, Some(&uav_desc), Some(&mut u))
            .map_err(|e| format!("CreateUnorderedAccessView: {e}"))?;
        u.unwrap()
    };

    Ok((tex, uav))
}
