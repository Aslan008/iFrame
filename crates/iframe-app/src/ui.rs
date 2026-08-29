//! iFrame UI: live frametime graph, limiter controls, game picker, tray.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use crate::live::{SharedState, HISTORY_SECONDS};
use crate::profiles::{GameProfile, Profiles};
use crate::sm_host::HostMapping;
use crate::{injector, live, sm_host, tray};
use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerMode;

pub struct IFrameApp {
    state: Arc<SharedState>,
    mapping: Option<HostMapping>,
    worker: Option<std::thread::JoinHandle<()>>,

    windows: Vec<(u32, String)>,
    selected_window: usize,
    exe_name: Option<String>,
    attached: bool,

    target_fps: f64,
    mode: PacerMode,
    vsync_override: bool,
    auto_attach: bool,
    always_on_top: bool,
    applied_on_top: bool,
    window_visible: bool,

    profiles: Profiles,
    tray: Option<tray::Tray>,
    hotkeys: Option<tray::HotKeys>,
    tray_tried: bool,
    last_auto_scan: Instant,
}

impl IFrameApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        let state = SharedState::new();
        let profiles = Profiles::load();
        let mut app = Self {
            state,
            mapping: None,
            worker: None,
            windows: Vec::new(),
            selected_window: 0,
            exe_name: None,
            attached: false,
            target_fps: 60.0,
            mode: PacerMode::FixedVsync,
            vsync_override: true,
            auto_attach: false,
            always_on_top: true,
            applied_on_top: false,
            window_visible: true,
            profiles,
            tray: None,  // created lazily on the first ui() frame (see below)
            hotkeys: None,
            tray_tried: false,
            last_auto_scan: Instant::now() - Duration::from_secs(10),
        };
        app.refresh_windows();
        app
    }

    // ----- attach / detach -------------------------------------------------

    fn attach(&mut self, pid: u32) {
        if self.attached {
            self.detach();
        }
        let mapping = match sm_host::create_for_pid(pid) {
            Ok(m) => m,
            Err(e) => {
                log_ui(&format!("attach failed: {e}"));
                return;
            }
        };
        if let Err(e) = injector::inject(pid, &injector::default_dll_path_for(pid)) {
            log_ui(&format!("inject failed: {e}"));
            return;
        }
        // Wait briefly for the hook to publish readiness.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if mapping.ring.hook_state() == 1 {
                break;
            }
            if mapping.ring.hook_state() == 2 || std::time::Instant::now() > deadline {
                log_ui("hook init failed (see %TEMP%\\iframe_hook.log)");
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }

        self.exe_name = process_exe_name(pid);
        if let Some(exe) = &self.exe_name {
            if let Some(p) = self.profiles.get(exe).cloned() {
                self.target_fps = p.target_fps;
                self.mode = match p.mode.as_str() {
                    "vrr" => PacerMode::Vrr,
                    "off" => PacerMode::Bypass,
                    _ => PacerMode::FixedVsync,
                };
                self.vsync_override = p.vsync_override;
                self.auto_attach = p.auto_attach;
            }
        }

        self.state.attached_pid.store(pid, Ordering::Relaxed);
        self.worker = Some(live::spawn_worker(pid, self.state.clone()));
        self.mapping = Some(mapping);
        self.attached = true;
        self.push_config();
    }

    fn detach(&mut self) {
        self.state.attached_pid.store(0, Ordering::Relaxed);
        if let Some(mapping) = &self.mapping {
            let cfg = RuntimeConfig {
                enabled: false,
                mode: self.mode,
                target_fps: self.target_fps,
                refresh_hz: 0.0,
            };
            mapping.ring.set_config(&cfg);
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        self.mapping = None;
        self.attached = false;
        self.exe_name = None;
    }

    /// Publish the current UI settings into the shared header.
    fn push_config(&mut self) {
        if let Some(mapping) = &self.mapping {
            let enabled = self.mode != PacerMode::Bypass && self.target_fps > 0.0;
            let cfg = RuntimeConfig {
                enabled,
                mode: self.mode,
                target_fps: self.target_fps,
                refresh_hz: 0.0, // 0 = the DLL trusts the DWM hint
            };
            mapping.ring.set_config(&cfg);
            self.state.limiter_on.store(enabled, Ordering::Relaxed);
            if let Some(exe) = self.exe_name.clone() {
                self.profiles.set(
                    &exe,
                    GameProfile {
                        target_fps: self.target_fps,
                        mode: match self.mode {
                            PacerMode::Vrr => "vrr".into(),
                            PacerMode::Bypass => "off".into(),
                            PacerMode::FixedVsync => "vsync".into(),
                        },
                        vsync_override: self.vsync_override,
                        auto_attach: self.auto_attach,
                    },
                );
            }
        }
    }

    // ----- background integrations -----------------------------------------

    fn poll_tray(&mut self, ctx: &egui::Context) {
        for ev in tray::poll_menu_events() {
            if let Some(t) = &self.tray {
                if ev.id == t.show_item.id() {
                    self.window_visible = !self.window_visible;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.window_visible));
                    if self.window_visible {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                } else if ev.id == t.quit_item.id() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else if ev.id == t.limiter_item.id() {
                    self.toggle_limiter();
                }
            }
        }
    }

    fn poll_hotkey(&mut self) {
        let toggle_id = self.hotkeys.as_ref().map(|hk| hk.toggle_limiter);
        if let Some(toggle_id) = toggle_id {
            for id in tray::poll_hotkey_events() {
                if id == toggle_id {
                    self.toggle_limiter();
                }
            }
        }
    }

    fn toggle_limiter(&mut self) {
        if self.mode == PacerMode::Bypass {
            self.mode = PacerMode::FixedVsync;
        } else {
            self.mode = PacerMode::Bypass;
        }
        self.push_config();
    }

    fn auto_scan(&mut self) {
        if !self.auto_attach
            || self.attached
            || self.last_auto_scan.elapsed() < Duration::from_secs(2)
        {
            return;
        }
        self.last_auto_scan = Instant::now();
        let windows = enumerate_windows();
        for (pid, _title) in windows {
            if let Some(exe) = process_exe_name(pid) {
                if let Some(profile) = self.profiles.get(&exe).cloned() {
                    if profile.auto_attach {
                        self.attach(pid);
                        break;
                    }
                }
            }
        }
    }

    fn refresh_windows(&mut self) {
        self.windows = enumerate_windows();
        if self.selected_window >= self.windows.len() {
            self.selected_window = 0;
        }
    }

    // ----- drawing ----------------------------------------------------------

    fn graph(&self, ui: &mut egui::Ui) {
        let target_us = if self.target_fps > 0.0 {
            1e6 / self.target_fps
        } else {
            0.0
        };
        let now_s = live_now_s();
        let mut points: Vec<[f64; 2]> = Vec::new();
        if let Ok(stats) = self.state.stats.try_lock() {
            for (t, ft) in &stats.samples {
                points.push([-(now_s - t), *ft]);
            }
        }

        let mut plot = Plot::new("frametime")
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .x_axis_label("seconds ago")
            .y_axis_label("frametime, µs");
        if target_us > 0.0 {
            plot = plot.include_y(target_us * 2.5);
        }
        plot.show(ui, |plot_ui| {
            if !points.is_empty() {
                plot_ui.line(
                    Line::new("frametime", PlotPoints::from(points))
                        .color(egui::Color32::from_rgb(0x30, 0xE0, 0x6A))
                        .width(1.5),
                );
            }
            if target_us > 0.0 {
                plot_ui.line(
                    Line::new(
                        "target",
                        PlotPoints::from(vec![
                            [-HISTORY_SECONDS, target_us],
                            [0.0, target_us],
                        ]),
                    )
                    .color(egui::Color32::from_rgb(0xE0, 0xA0, 0x30))
                    .style(egui_plot::LineStyle::Dashed { length: 4.0 })
                    .width(1.0),
                );
            }
        });
    }
}

impl eframe::App for IFrameApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tray(ctx);
        self.poll_hotkey();
        self.auto_scan();

        if self.always_on_top != self.applied_on_top {
            let level = if self.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            self.applied_on_top = self.always_on_top;
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Lazy tray/hotkey init: by the first ui() frame the winit event loop
        // is pumping messages, so the tray's hidden window is serviced
        // reliably. Creating them before the loop raced it (one-shot crash).
        if !self.tray_tried {
            self.tray_tried = true;
            self.tray = tray::create_tray().ok();
            self.hotkeys = tray::create_hotkeys().ok();
        }

        let attached_pid = self.state.attached_pid.load(Ordering::Relaxed);

        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("iFrame");
                ui.separator();
                if attached_pid != 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                        format!("● attached {attached_pid}"),
                    );
                } else {
                    ui.weak("○ not attached");
                }
                if let Some(exe) = &self.exe_name {
                    ui.weak(exe);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Applied in logic() via the viewport command.
                    ui.toggle_value(&mut self.always_on_top, "📌 on top");
                });
            });
        });

        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.weak(
                    "Ctrl+Alt+I — toggle limiter · tray: show/hide · profiles in %APPDATA%\\iFrame",
                );
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            // --- graph ---
            let graph_h = (ui.available_height() * 0.52).max(140.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_size(egui::vec2(ui.available_width(), graph_h));
                self.graph(ui);
            });
            ui.add_space(6.0);

            // --- live stats ---
            let (fps, p50, p99, late, total) = self
                .state
                .stats
                .try_lock()
                .map(|s| (s.fps, s.p50_us, s.p99_us, s.late, s.total))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                stat(ui, "FPS", &format!("{fps:.1}"));
                stat(ui, "p50", &format!("{p50:.2} ms"));
                stat(ui, "p99", &format!("{p99:.2} ms"));
                stat(ui, "late", &format!("{late}"));
                stat(ui, "frames", &format!("{total}"));
            });
            ui.add_space(6.0);

            // --- limiter controls ---
            ui.horizontal(|ui| {
                ui.label("Target FPS");
                if ui
                    .add(
                        egui::DragValue::new(&mut self.target_fps)
                            .speed(0.5)
                            .range(10.0..=480.0),
                    )
                    .changed()
                {
                    self.push_config();
                }
                for preset in [30.0, 40.0, 60.0] {
                    if ui.button(format!("{preset:.0}")).clicked() {
                        self.target_fps = preset;
                        self.push_config();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Mode:");
                let mut changed = false;
                changed |= ui
                    .radio_value(&mut self.mode, PacerMode::FixedVsync, "ZeroLag (VSync grid)")
                    .changed();
                changed |= ui
                    .radio_value(&mut self.mode, PacerMode::Vrr, "VRR (start-to-start)")
                    .changed();
                changed |= ui
                    .radio_value(&mut self.mode, PacerMode::Bypass, "Off")
                    .changed();
                if ui
                    .checkbox(&mut self.vsync_override, "override VSync")
                    .changed()
                {
                    changed = true;
                }
                if changed {
                    self.push_config();
                }
            });
            ui.add_space(6.0);

            // --- attach controls ---
            ui.horizontal(|ui| {
                if ui.button("⟳").clicked() {
                    self.refresh_windows();
                }
                let selected = self
                    .windows
                    .get(self.selected_window)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| "— pick a window —".into());
                egui::ComboBox::from_id_salt("game")
                    .selected_text(selected)
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (i, (_, title)) in self.windows.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_window, i, title);
                        }
                    });
                if self.attached {
                    if ui.button("Detach").clicked() {
                        self.detach();
                    }
                } else if ui.button("Attach").clicked() {
                    if let Some(pid) = self.windows.get(self.selected_window).map(|(p, _)| *p) {
                        self.attach(pid);
                    }
                }
                if ui
                    .checkbox(&mut self.auto_attach, "auto-attach known games")
                    .changed()
                {
                    if let Some(exe) = self.exe_name.clone() {
                        let mut p = self.profiles.get(&exe).cloned().unwrap_or_default();
                        p.auto_attach = self.auto_attach;
                        self.profiles.set(&exe, p);
                    }
                }
            });
        });
    }
}

impl Drop for IFrameApp {
    fn drop(&mut self) {
        // Never leave the limiter running with a dead control app.
        self.state.attached_pid.store(0, Ordering::Relaxed);
        if let Some(mapping) = &self.mapping {
            let cfg = RuntimeConfig {
                enabled: false,
                mode: self.mode,
                target_fps: self.target_fps,
                refresh_hz: 0.0,
            };
            mapping.ring.set_config(&cfg);
        }
    }
}

// ----- helpers --------------------------------------------------------------

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.weak(label);
        ui.monospace(value);
    });
    ui.separator();
}

fn live_now_s() -> f64 {
    let (mut v, mut f) = (0i64, 0i64);
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut v);
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 { 0.0 } else { v as f64 / f as f64 }
}

fn default_dll_path() -> std::path::PathBuf {
    std::path::PathBuf::from("target/release/iframe_hook.dll")
}

fn enumerate_windows() -> Vec<(u32, String)> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    let out: Arc<std::sync::Mutex<Vec<(u32, String)>>> = Arc::default();
    let sink = out.clone();
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let sink: &Arc<std::sync::Mutex<Vec<(u32, String)>>> =
            unsafe { &*(lparam.0 as *const Arc<std::sync::Mutex<Vec<(u32, String)>>>) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len == 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Ok(mut list) = sink.lock() {
                list.push((pid, title));
            }
            BOOL(1)
        }
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(cb),
            LPARAM(Arc::as_ptr(&sink) as isize),
        );
    }
    out.lock().map(|l| l.clone()).unwrap_or_default()
}

fn process_exe_name(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if !ok || size == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
    }
}

fn log_ui(msg: &str) {
    eprintln!("[iFrame] {msg}");
}

/// Entry point for the GUI mode.
pub fn run() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 640.0])
            .with_min_inner_size([380.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "iFrame — zero-lag frame pacer",
        options,
        Box::new(|cc| Ok(Box::new(IFrameApp::new(cc)))),
    )
}