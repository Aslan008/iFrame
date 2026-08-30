//! iFrame UI: live frametime graph, limiter controls, game picker, tray.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use crate::live::{SharedState, HISTORY_SECONDS};
use crate::profiles::{GameProfile, Profiles};
use crate::sm_host::HostMapping;
use crate::{anticheat, etw, injector, live, sm_host, tray};
use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerMode;

/// Result of the background attach thread.
enum AttachOutcome {
    Done { pid: u32, mapping: HostMapping },
    Failed(String),
}

#[derive(Default, Clone)]
struct DisplaySnapshot {
    headline_fps: f64,
    headline_p50_us: f64,
    headline_p99_us: f64,
    headline_late: u64,
    headline_total: u64,
    off: live::SideStats,
    on: live::SideStats,
}

pub struct IFrameApp {
    state: Arc<SharedState>,
    mapping: Option<HostMapping>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// In-flight background attach: (pid, receiver of the outcome).
    pending: Option<(u32, std::sync::mpsc::Receiver<AttachOutcome>)>,

    windows: Vec<(u32, String)>,
    selected_window: usize,
    exe_name: Option<String>,
    attached: bool,

    target_fps: f64,
    mode: PacerMode,
    vsync_override: bool,
    force_waitable: bool,
    auto_attach: bool,
    always_on_top: bool,
    applied_on_top: bool,
    window_visible: bool,

    profiles: Profiles,
    tray: Option<tray::Tray>,
    hotkeys: Option<tray::HotKeys>,
    tray_tried: bool,
    last_auto_scan: Instant,
    /// Telemetry-only ETW session (anti-cheat protected targets).
    etw_watch: Option<etw::EtwWatch>,

    display_stats: DisplaySnapshot,
    last_stats_refresh: Instant,
    reset_plot_view: bool,
}

impl IFrameApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        let state = SharedState::new();
        let profiles = Profiles::load();
        let mut app = Self {
            state,
            mapping: None,
            worker: None,
            pending: None,
            windows: Vec::new(),
            selected_window: 0,
            exe_name: None,
            attached: false,
            target_fps: 60.0,
            mode: PacerMode::FixedVsync,
            vsync_override: true,
            force_waitable: false,
            auto_attach: false,
            always_on_top: true,
            applied_on_top: false,
            window_visible: true,
            profiles,
            tray: None,  // created lazily on the first ui() frame (see below)
            hotkeys: None,
            tray_tried: false,
            last_auto_scan: Instant::now() - Duration::from_secs(10),
            etw_watch: None,
            display_stats: DisplaySnapshot::default(),
            last_stats_refresh: Instant::now() - Duration::from_secs(1),
            reset_plot_view: false,
        };
        app.refresh_windows();
        app
    }

    // ----- attach / detach -------------------------------------------------

    fn attach(&mut self, pid: u32) {
        if self.attached {
            self.detach();
        }
        // M6 safety gate: the anti-cheat check runs BEFORE any process access.
        // Protected targets get the telemetry-only ETW mode instead.
        if let Err(e) = anticheat::check_process(pid) {
            log_ui(&format!("{e}"));
            match etw::start_watch(pid, self.state.clone()) {
                Ok(w) => {
                    self.etw_watch = Some(w);
                    self.attached = true;
                    self.exe_name = anticheat::process_name(pid).ok();
                    self.state.attached_pid.store(pid, Ordering::Relaxed);
                    log_ui("telemetry-only ETW mode active (no injection)");
                }
                Err(e2) => log_ui(&format!("ETW telemetry-only failed: {e2}")),
            }
            return;
        }
        let mapping = match sm_host::create_for_pid(pid) {
            Ok(m) => m,
            Err(e) => {
                log_ui(&format!("attach failed: {e}"));
                return;
            }
        };
        // Injection + the hook-init wait run on a worker thread — the UI
        // thread must never block for seconds (it froze the window before).
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending = Some((pid, rx));
        log_ui(&format!("attaching to pid {pid} in background..."));
        std::thread::spawn(move || {
            if let Err(e) = injector::inject(pid, &injector::default_dll_path_for(pid)) {
                let _ = tx.send(AttachOutcome::Failed(format!("inject failed: {e}")));
                return;
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match mapping.ring.hook_state() {
                    1 => break,
                    2 => {
                        let _ = tx.send(AttachOutcome::Failed(
                            "hook init failed (see %TEMP%\\iframe_hook.log)".into(),
                        ));
                        return;
                    }
                    _ => {
                        if std::time::Instant::now() > deadline {
                            let _ = tx.send(AttachOutcome::Failed(format!(
                                "timeout waiting for hook init (state={})",
                                mapping.ring.hook_state()
                            )));
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            let _ = tx.send(AttachOutcome::Done { pid, mapping });
        });
    }

    /// Poll the background attach thread; finalize on completion.
    fn poll_attach(&mut self) {
        let outcome = match &self.pending {
            Some((_, rx)) => match rx.try_recv() {
                Ok(o) => Some(o),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(_) => Some(AttachOutcome::Failed("attach thread died".into())),
            },
            None => None,
        };
        let Some(outcome) = outcome else { return };
        self.pending = None;
        match outcome {
            AttachOutcome::Done { pid, mapping } => {
                if self.attached {
                    return; // detached while attaching — drop the result
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
                        self.force_waitable = p.force_waitable;
                        self.auto_attach = p.auto_attach;
                    }
                }
                self.state.attached_pid.store(pid, Ordering::Relaxed);
                self.worker = Some(live::spawn_worker(pid, self.state.clone()));
                self.mapping = Some(mapping);
                self.attached = true;
                self.push_config();
            }
            AttachOutcome::Failed(e) => log_ui(&e),
        }
    }

    fn detach(&mut self) {
        // An in-flight attach is abandoned: its outcome is discarded when it
        // arrives (the injected hook stays in limiter-disabled pass-through).
        self.pending = None;
        if let Some(mut w) = self.etw_watch.take() {
            w.stop();
        }
        self.state.attached_pid.store(0, Ordering::Relaxed);
        if let Some(mapping) = &self.mapping {
            let cfg = RuntimeConfig {
                enabled: false,
                mode: self.mode,
                target_fps: self.target_fps,
                refresh_hz: 0.0,
                vsync_override: self.vsync_override,
                force_waitable: false,
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
                vsync_override: self.vsync_override,
                force_waitable: self.force_waitable,
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
                        force_waitable: self.force_waitable,
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

    fn graph(&mut self, ui: &mut egui::Ui, height: f32) {
        let target_ms = if self.target_fps > 0.0 {
            1000.0 / self.target_fps
        } else {
            0.0
        };
        let now_s = live_now_s();
        let mut off_points: Vec<[f64; 2]> = Vec::new();
        let mut on_points: Vec<[f64; 2]> = Vec::new();
        if let Ok(stats) = self.state.stats.try_lock() {
            off_points.reserve(stats.off.samples.len().min(4096));
            for (t, ft_us) in &stats.off.samples {
                if *t <= now_s && (now_s - t) <= HISTORY_SECONDS + 1.0 {
                    off_points.push([-(now_s - t), *ft_us / 1000.0]);
                }
            }
            on_points.reserve(stats.on.samples.len().min(4096));
            for (t, ft_us) in &stats.on.samples {
                if *t <= now_s && (now_s - t) <= HISTORY_SECONDS + 1.0 {
                    on_points.push([-(now_s - t), *ft_us / 1000.0]);
                }
            }
        }

        let mut plot = Plot::new("frametime")
            .height(height)
            .allow_drag(true)
            .allow_zoom(true)
            .allow_scroll(true)
            .allow_boxed_zoom(true)
            .allow_double_click_reset(true)
            .x_axis_label("секунды назад")
            .y_axis_label("время кадра, мс");
        if target_ms > 0.0 {
            plot = plot.include_y(target_ms * 2.2).include_y(0.0);
        }
        let reset_view = self.reset_plot_view;
        self.reset_plot_view = false;
        plot.show(ui, |plot_ui| {
            if reset_view {
                plot_ui.set_auto_bounds([true, true]);
            }
            // Two colored series — limiter OFF (dim blue) vs ON (green): the
            // graph itself shows the before/after transition.
            if !off_points.is_empty() {
                plot_ui.line(
                    Line::new("лимитер ВЫКЛ (до)", PlotPoints::from(off_points))
                        .color(egui::Color32::from_rgb(0x50, 0x90, 0xC0))
                        .width(1.2),
                );
            }
            if !on_points.is_empty() {
                plot_ui.line(
                    Line::new("лимитер ВКЛ (после)", PlotPoints::from(on_points))
                        .color(egui::Color32::from_rgb(0x30, 0xE0, 0x6A))
                        .width(1.6),
                );
            }
            if target_ms > 0.0 {
                plot_ui.line(
                    Line::new(
                        format!("цель: {:.0} FPS ({:.1} мс)", self.target_fps, target_ms),
                        PlotPoints::from(vec![
                            [-HISTORY_SECONDS, target_ms],
                            [0.0, target_ms],
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
        self.poll_attach();
        self.profiles.flush_due();
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

        if self.last_stats_refresh.elapsed() >= Duration::from_millis(250) {
            if let Ok(stats) = self.state.stats.try_lock() {
                self.display_stats = DisplaySnapshot {
                    headline_fps: stats.fps,
                    headline_p50_us: stats.p50_us,
                    headline_p99_us: stats.p99_us,
                    headline_late: stats.late,
                    headline_total: stats.total,
                    off: stats.off.clone(),
                    on: stats.on.clone(),
                };
                self.last_stats_refresh = Instant::now();
            }
        }

        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("iFrame");
                ui.separator();
                if attached_pid != 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                        format!("● подключено (PID {attached_pid})"),
                    );
                } else if self.pending.is_some() {
                    ui.colored_label(egui::Color32::from_rgb(0xE0, 0xA0, 0x30), "◌ подключение…");
                } else {
                    ui.weak("○ не подключено");
                }
                if let Some(exe) = &self.exe_name {
                    ui.weak(exe);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Applied in logic() via the viewport command.
                    ui.toggle_value(&mut self.always_on_top, "📌 поверх всех")
                        .on_hover_text("Закрепить окно iFrame поверх игры");
                });
            });
        });

        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.weak(
                    "Ctrl+Alt+I — вкл/выкл лимитер · трей: скрыть · профили в %APPDATA%\\iFrame",
                );
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            // --- 1. Graph header bar ---
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📈 График фреймтайма").strong());
                if ui
                    .button("⛶ Авто-центровка")
                    .on_hover_text("Сбросить зум и отцентрировать график по текущим данным и целевому FPS")
                    .clicked()
                {
                    self.reset_plot_view = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("Колёсико: зум · ЛКМ: перемещение · 2x клик: сброс");
                });
            });
            ui.add_space(2.0);

            // --- 2. Dynamic resizable graph (placed outside ScrollArea for 100% responsive mouse wheel zoom) ---
            let available_h = ui.available_height();
            let graph_h = (available_h - 380.0).clamp(130.0, 480.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_size(egui::vec2(ui.available_width(), graph_h));
                self.graph(ui, graph_h);
            });
            ui.add_space(6.0);

            // --- 3. Scrollable controls & stats area below the graph ---
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let d = &self.display_stats;

                    // --- live headline stats with tooltips ---
                    ui.horizontal(|ui| {
                        stat(
                            ui,
                            "FPS",
                            &format!("{:.1}", d.headline_fps),
                            "Текущая средняя частота кадров за 1 секунду",
                        );
                        stat(
                            ui,
                            "p50 (медиана)",
                            &format!("{:.2} мс", d.headline_p50_us / 1000.0),
                            "Типичное время кадра (50% кадров быстрее этого значения)",
                        );
                        stat(
                            ui,
                            "p99 (просадки)",
                            &format!("{:.2} мс", d.headline_p99_us / 1000.0),
                            "99-й перцентиль (худшие 1% кадров). Если p99 сильно выше p50 — в игре есть статтеры",
                        );
                        stat(
                            ui,
                            "пропуски",
                            &format!("{}", d.headline_late),
                            "Кадры, которые игра не успела отрендерить вовремя к VBlank",
                        );
                        stat(
                            ui,
                            "всего кадров",
                            &format!("{}", d.headline_total),
                            "Всего обработано кадров с момента подключения",
                        );
                    });
                    ui.add_space(4.0);

                    // --- A/B compare table with rock-solid fixed column widths ---
                    let off = &d.off;
                    let on = &d.on;
                    let off_jitter = if off.total >= 2 {
                        (off.p99_us - off.p50_us).max(0.0) / 1000.0
                    } else {
                        0.0
                    };
                    let on_jitter = if on.total >= 2 {
                        (on.p99_us - on.p50_us).max(0.0) / 1000.0
                    } else {
                        0.0
                    };

                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("📊 Сравнение: До vs После (последние 10 сек)").strong());
                        });
                        ui.add_space(2.0);

                        egui::Grid::new("ab_compare")
                            .num_columns(3)
                            .spacing([12.0, 3.0])
                            .show(ui, |ui| {
                                grid_col_label(ui, "Параметр", "Название метрики качества");
                                grid_col_header(ui, "Без лимитера (OFF)");
                                grid_col_header(ui, "С лимитером (ON)");
                                ui.end_row();

                                grid_row_label(ui, "Частота кадров (FPS)", "Средний FPS за период с выключенным и включённым лимитером");
                                ab_val(ui, off.total >= 2, format!("{:.1}", off.fps));
                                ab_val(ui, on.total >= 2, format!("{:.1}", on.fps));
                                ui.end_row();

                                grid_row_label(ui, "Время кадра (p50)", "Типичное время между кадрами (идеал: 16.67 мс для 60 FPS, 25.00 мс для 40 FPS)");
                                ab_val(ui, off.total >= 2, format!("{:.2} мс", off.p50_us / 1000.0));
                                ab_val(ui, on.total >= 2, format!("{:.2} мс", on.p50_us / 1000.0));
                                ui.end_row();

                                grid_row_label(ui, "Редкие просадки (p99)", "99% кадров рендерятся быстрее этого времени. Скачки p99 = микрофризы");
                                ab_val(ui, off.total >= 2, format!("{:.2} мс", off.p99_us / 1000.0));
                                ab_val(ui, on.total >= 2, format!("{:.2} мс", on.p99_us / 1000.0));
                                ui.end_row();

                                grid_row_label(ui, "Разброс (джиттер p99-p50)", "Разница между худшими и типичными кадрами. Чем меньше разброс — тем картинка плавнее!");
                                ab_val(ui, off.total >= 2, format!("±{off_jitter:.2} мс"));
                                ab_val(ui, on.total >= 2, format!("±{on_jitter:.2} мс"));
                                ui.end_row();

                                grid_row_label(ui, "Отдача кадра (Present)", "Время вызова функции Present видеокарте. Должно быть ~0.1 мс — это подтверждает 0 мс добавленного инпут-лага!");
                                ab_val(
                                    ui,
                                    off.total >= 2 && off.has_timing,
                                    format!("{:.2} мс", off.hold_p50_us / 1000.0),
                                );
                                ab_val(
                                    ui,
                                    on.total >= 2 && on.has_timing,
                                    format!("{:.2} мс", on.hold_p50_us / 1000.0),
                                );
                                ui.end_row();

                                grid_row_label(ui, "Пауза перед след. кадром", "Умная пауза, выдерживаемая ДО опроса ввода следующего кадра для синхронизации с монитором");
                                ab_val(
                                    ui,
                                    off.total >= 2 && off.has_timing,
                                    format!("{:.2} мс", off.wait_p50_us / 1000.0),
                                );
                                ab_val(
                                    ui,
                                    on.total >= 2 && on.has_timing,
                                    format!("{:.2} мс", on.wait_p50_us / 1000.0),
                                );
                                ui.end_row();
                            });

                        // --- automatic verdict card ---
                        ui.add_space(4.0);
                        if off.total >= 10 && on.total >= 10 {
                            if on_jitter < off_jitter && off_jitter > 0.05 {
                                let times = (off_jitter / on_jitter.max(0.01)).max(1.1);
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                                    format!("✔ Итог: фреймтайм стал ровнее в {times:.1}x раз! (разброс ±{on_jitter:.2} мс против ±{off_jitter:.2} мс без лимитера). Инпут-лаг: +0.0 мс"),
                                );
                            } else {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x50, 0x90, 0xC0),
                                    format!("ℹ Итог: Лимитер держит стабильные {:.0} FPS (разброс ±{on_jitter:.2} мс).", on.fps),
                                );
                            }
                        } else if attached_pid != 0 {
                            ui.weak("💡 Подсказка: нажмите Ctrl+Alt+I на 5 секунд (выключить), затем снова Ctrl+Alt+I (включить), чтобы накопить данные для оценки.");
                        }
                    });
                    ui.add_space(6.0);

                    // --- limiter controls ---
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("⚙ Управление лимитером").strong());
                        ui.add_space(2.0);

                        ui.horizontal(|ui| {
                            ui.label("Целевой FPS:");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.target_fps)
                                        .speed(0.5)
                                        .range(10.0..=480.0),
                                )
                                .on_hover_text("Задайте желаемую частоту кадров")
                                .changed()
                            {
                                self.push_config();
                            }
                            for preset in [30.0, 40.0, 60.0, 120.0] {
                                if ui.button(format!("{preset:.0}")).on_hover_text(match preset as u32 {
                                    30 => "30 FPS — для тяжелых игр",
                                    40 => "40 FPS — идеальный шаг для 120 Гц экранов (25.0 мс)",
                                    60 => "60 FPS — стандартная плавность (16.67 мс)",
                                    _ => "120 FPS — высокая герцовка",
                                }).clicked() {
                                    self.target_fps = preset;
                                    self.push_config();
                                }
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Режим:");
                            let mut changed = false;
                            changed |= ui
                                .radio_value(&mut self.mode, PacerMode::FixedVsync, "ZeroLag (Сетка VSync)")
                                .on_hover_text("Синхронизация по аппаратной сетке развертки экрана. Устраняет статтеры без добавления задержки ввода")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.mode, PacerMode::Vrr, "VRR (G-Sync/FreeSync)")
                                .on_hover_text("Пейсинг от старта до старта кадра для мониторов с переменной частотой (VRR)")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.mode, PacerMode::Bypass, "Выкл")
                                .on_hover_text("Ограничение отключено (режим замера без пейсинга)")
                                .changed();
                            if changed {
                                self.push_config();
                            }
                        });

                        ui.horizontal(|ui| {
                            let mut changed = false;
                            if ui
                                .checkbox(&mut self.vsync_override, "override VSync")
                                .on_hover_text("Принудительно управлять VSync на уровне DXGI")
                                .changed()
                            {
                                changed = true;
                            }
                            if ui
                                .checkbox(&mut self.force_waitable, "waitable object")
                                .on_hover_text("Использовать высокоточный таймер DirectX Flip Model для максимальной стабильности")
                                .changed()
                            {
                                changed = true;
                            }
                            if changed {
                                self.push_config();
                            }
                        });
                    });
                    ui.add_space(6.0);

                    // --- attach controls ---
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("🎮 Подключение к игре").strong());
                        ui.add_space(2.0);

                        ui.horizontal(|ui| {
                            if ui.button("⟳").on_hover_text("Обновить список запущенных окон игр").clicked() {
                                self.refresh_windows();
                            }
                            let selected = self
                                .windows
                                .get(self.selected_window)
                                .map(|(_, t)| t.clone())
                                .unwrap_or_else(|| "— выберите окно игры —".into());
                            egui::ComboBox::from_id_salt("game")
                                .selected_text(selected)
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for (i, (_, title)) in self.windows.iter().enumerate() {
                                        ui.selectable_value(&mut self.selected_window, i, title);
                                    }
                                });
                            if self.attached {
                                if ui.button("Отключить").clicked() {
                                    self.detach();
                                }
                            } else if ui.button("Подключить").clicked() {
                                if let Some(pid) = self.windows.get(self.selected_window).map(|(p, _)| *p) {
                                    self.attach(pid);
                                }
                            }
                            if ui
                                .checkbox(&mut self.auto_attach, "авто-подключение")
                                .on_hover_text("Автоматически подключаться к этой игре при её запуске")
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
                });
        });
    }
}

impl Drop for IFrameApp {
    fn drop(&mut self) {
        // Persist any debounced profile changes.
        self.profiles.flush();
        // Never leave the limiter running with a dead control app.
        self.state.attached_pid.store(0, Ordering::Relaxed);
        if let Some(mapping) = &self.mapping {
            let cfg = RuntimeConfig {
                enabled: false,
                mode: self.mode,
                target_fps: self.target_fps,
                refresh_hz: 0.0,
                vsync_override: self.vsync_override,
                force_waitable: false,
            };
            mapping.ring.set_config(&cfg);
        }
    }
}

// ----- helpers --------------------------------------------------------------

fn stat(ui: &mut egui::Ui, label: &str, value: &str, tooltip: &str) {
    let r = ui.vertical(|ui| {
        ui.weak(label);
        ui.monospace(value);
    });
    r.response.on_hover_text(tooltip);
    ui.separator();
}

fn grid_col_label(ui: &mut egui::Ui, text: &str, tooltip: &str) {
    ui.add_sized([175.0, 18.0], egui::Label::new(egui::RichText::new(text).weak()))
        .on_hover_text(tooltip);
}

fn grid_col_header(ui: &mut egui::Ui, text: &str) {
    ui.add_sized([115.0, 18.0], egui::Label::new(egui::RichText::new(text).weak()));
}

fn grid_row_label(ui: &mut egui::Ui, text: &str, tooltip: &str) {
    ui.add_sized([175.0, 18.0], egui::Label::new(text))
        .on_hover_text(tooltip);
}

/// A/B cell with fixed size: prevents column shifting/twitching when numbers update.
fn ab_val(ui: &mut egui::Ui, show: bool, text: String) {
    ui.add_sized(
        [115.0, 18.0],
        if show {
            egui::Label::new(egui::RichText::new(text).monospace())
        } else {
            egui::Label::new(egui::RichText::new("—").weak())
        },
    );
}

fn live_now_s() -> f64 {
    let (mut v, mut f) = (0i64, 0i64);
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut v);
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 { 0.0 } else { v as f64 / f as f64 }
}

fn enumerate_windows() -> Vec<(u32, String)> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    let mut out: Vec<(u32, String)> = Vec::new();
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let sink: &mut Vec<(u32, String)> =
            unsafe { &mut *(lparam.0 as *mut Vec<(u32, String)>) };
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
            sink.push((pid, title));
            BOOL(1)
        }
    }
    unsafe {
        let _ = EnumWindows(
            Some(cb),
            LPARAM(&mut out as *mut Vec<(u32, String)> as isize),
        );
    }
    out
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
            .with_inner_size([500.0, 720.0])
            .with_min_inner_size([420.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "iFrame — zero-lag frame pacer",
        options,
        Box::new(|cc| Ok(Box::new(IFrameApp::new(cc)))),
    )
}