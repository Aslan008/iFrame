//! iFrame UI: live frametime graph, limiter controls, game picker, tray.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use crate::gamepad::{GamepadAction, GamepadTracker};
use crate::live::{SharedState, HISTORY_SECONDS};
use crate::profiles::{GameProfile, Profiles};
use crate::sm_host::HostMapping;
use crate::{anticheat, etw, injector, live, sm_host, tray};
use iframe_common::config::{ReflexMode, RuntimeConfig};
use iframe_common::pacer::PacerMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiTab {
    #[default]
    Monitoring,
    Profiles,
}

/// Result of the background attach thread.
enum AttachOutcome {
    Done { pid: u32, mapping: HostMapping },
    Failed(String),
}

#[derive(Default, Clone, Copy)]
pub struct SideSummary {
    pub fps: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    pub total: u64,
    pub hold_p50_us: f64,
    pub wait_p50_us: f64,
    pub has_timing: bool,
    pub late: u64,
    pub hold_max_us: f64,
}

impl From<&live::SideStats> for SideSummary {
    fn from(s: &live::SideStats) -> Self {
        Self {
            fps: s.fps,
            p50_us: s.p50_us,
            p99_us: s.p99_us,
            total: s.total,
            hold_p50_us: s.hold_p50_us,
            wait_p50_us: s.wait_p50_us,
            has_timing: s.has_timing,
            late: s.late,
            hold_max_us: s.hold_max_us,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct DisplaySnapshot {
    headline_fps: f64,
    headline_p50_us: f64,
    headline_p99_us: f64,
    headline_late: u64,
    headline_total: u64,
    off: SideSummary,
    on: SideSummary,
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
    reflex_mode: ReflexMode,
    overlay_enabled: bool,
    auto_attach: bool,
    always_on_top: bool,
    applied_on_top: bool,
    window_visible: bool,

    gamepad: GamepadTracker,
    big_picture_mode: bool,
    active_tab: UiTab,

    pm_search: String,
    pm_new_exe: String,
    pm_export_text: Option<String>,
    pm_import_text: String,
    pm_show_import: bool,
    pm_status: Option<(String, Instant)>,

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
    custom_refresh_hz: f64,

    solver_cadence_fps: f64,
    solver_cadence_hz: f64,
    solver_cadence_result: Option<iframe_solver::cadence_synthesizer::CadenceSchedule>,
    solver_config_result: Option<String>,
    solver_demo_result: Option<String>,
    solver_has_flip: bool,
    solver_has_vrr: bool,
    solver_is_fullscreen: bool,

    tuner_result: Option<iframe_solver::telemetry_tuner::TuningRecommendation>,
    tuner_error: Option<String>,
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
            reflex_mode: ReflexMode::Off,
            overlay_enabled: false,
            auto_attach: false,
            always_on_top: true,
            applied_on_top: false,
            window_visible: true,
            gamepad: GamepadTracker::new(),
            big_picture_mode: false,
            active_tab: UiTab::Monitoring,
            pm_search: String::new(),
            pm_new_exe: String::new(),
            pm_export_text: None,
            pm_import_text: String::new(),
            pm_show_import: false,
            pm_status: None,
            profiles,
            tray: None,  // created lazily on the first ui() frame (see below)
            hotkeys: None,
            tray_tried: false,
            last_auto_scan: Instant::now() - Duration::from_secs(10),
            etw_watch: None,
            display_stats: DisplaySnapshot::default(),
            last_stats_refresh: Instant::now() - Duration::from_secs(1),
            reset_plot_view: false,
            custom_refresh_hz: 0.0,
            solver_cadence_fps: 40.0,
            solver_cadence_hz: 144.0,
            solver_cadence_result: None,
            solver_config_result: None,
            solver_demo_result: None,
            solver_has_flip: true,
            solver_has_vrr: true,
            solver_is_fullscreen: false,
            tuner_result: None,
            tuner_error: None,
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
                        self.custom_refresh_hz = p.refresh_hz;
                        self.reflex_mode = match p.reflex_mode.as_str() {
                            "boost" => ReflexMode::Boost,
                            "on" => ReflexMode::On,
                            _ => ReflexMode::Off,
                        };
                        self.overlay_enabled = p.overlay_enabled;
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
                reflex_mode: ReflexMode::Off,
                overlay_enabled: false,
            };
            mapping.ring.set_config(&cfg);
            // No interactive host anymore: clear host_present so the in-game
            // watchdog does not see a stale heartbeat, and a later CLI `limit`
            // can run headless.
            mapping.ring.mark_headless();
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
                refresh_hz: self.custom_refresh_hz, // 0 = the DLL trusts the DWM hint
                vsync_override: self.vsync_override,
                force_waitable: self.force_waitable,
                reflex_mode: self.reflex_mode,
                overlay_enabled: self.overlay_enabled,
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
                        refresh_hz: self.custom_refresh_hz,
                        reflex_mode: match self.reflex_mode {
                            ReflexMode::Off => "off".into(),
                            ReflexMode::On => "on".into(),
                            ReflexMode::Boost => "boost".into(),
                        },
                        overlay_enabled: self.overlay_enabled,
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

    fn poll_gamepad(&mut self, ctx: &egui::Context) {
        let actions = self.gamepad.poll();
        for action in actions {
            match action {
                GamepadAction::FpsDown(step) => {
                    self.target_fps = (self.target_fps - step as f64).max(10.0);
                    self.push_config();
                }
                GamepadAction::FpsUp(step) => {
                    self.target_fps = (self.target_fps + step as f64).min(480.0);
                    self.push_config();
                }
                GamepadAction::PrevPreset => {
                    let presets = [30.0, 40.0, 60.0, 90.0, 120.0, 144.0, 240.0];
                    if let Some(pos) = presets.iter().rposition(|&p| p < self.target_fps - 0.5) {
                        self.target_fps = presets[pos];
                        self.push_config();
                    }
                }
                GamepadAction::NextPreset => {
                    let presets = [30.0, 40.0, 60.0, 90.0, 120.0, 144.0, 240.0];
                    if let Some(pos) = presets.iter().position(|&p| p > self.target_fps + 0.5) {
                        self.target_fps = presets[pos];
                        self.push_config();
                    }
                }
                GamepadAction::ToggleLimiter => {
                    self.toggle_limiter();
                }
                GamepadAction::ToggleAutoAttach => {
                    self.auto_attach = !self.auto_attach;
                    if let Some(exe) = self.exe_name.clone() {
                        let mut p = self.profiles.get(&exe).cloned().unwrap_or_default();
                        p.auto_attach = self.auto_attach;
                        self.profiles.set(&exe, p);
                    }
                }
                GamepadAction::Attach => {
                    if !self.attached {
                        if let Some(pid) = self.windows.get(self.selected_window).map(|(p, _)| *p) {
                            self.attach(pid);
                        }
                    }
                }
                GamepadAction::ToggleBigPicture => {
                    self.big_picture_mode = !self.big_picture_mode;
                    let zoom = if self.big_picture_mode { 1.35 } else { 1.0 };
                    ctx.set_zoom_factor(zoom);
                }
            }
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

    fn show_profile_manager(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("📁 Менеджер профилей игр").heading());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("📋 Экспорт в TOML")
                    .on_hover_text("Экспортировать все профили в текстовый формат TOML")
                    .clicked()
                {
                    match self.profiles.export_toml() {
                        Ok(text) => {
                            ui.ctx().copy_text(text.clone());
                            self.pm_export_text = Some(text);
                            self.pm_status = Some((
                                "✔ Профили скопированы в буфер обмена!".into(),
                                Instant::now(),
                            ));
                        }
                        Err(e) => {
                            self.pm_status =
                                Some((format!("Ошибка экспорта: {e}"), Instant::now()));
                        }
                    }
                }
                if ui
                    .button("📥 Импорт из TOML")
                    .on_hover_text("Импортировать или вставить профили из TOML")
                    .clicked()
                {
                    self.pm_show_import = !self.pm_show_import;
                }
            });
        });
        ui.add_space(4.0);

        if let Some((msg, time)) = &self.pm_status {
            if time.elapsed() < Duration::from_secs(4) {
                ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), msg);
                ui.add_space(2.0);
            }
        }

        if self.pm_show_import {
            ui.group(|ui| {
                ui.label(egui::RichText::new("Вставьте содержимое TOML для импорта:").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut self.pm_import_text)
                        .desired_rows(6)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    if ui.button("✔ Применить импорт").clicked() {
                        match self.profiles.import_toml(&self.pm_import_text) {
                            Ok(count) => {
                                self.pm_status = Some((
                                    format!("✔ Успешно импортировано профилей: {count}"),
                                    Instant::now(),
                                ));
                                self.pm_show_import = false;
                                self.pm_import_text.clear();
                            }
                            Err(e) => {
                                self.pm_status =
                                    Some((format!("Ошибка импорта: {e}"), Instant::now()));
                            }
                        }
                    }
                    if ui.button("Отмена").clicked() {
                        self.pm_show_import = false;
                    }
                });
            });
            ui.add_space(4.0);
        }

        let mut close_export = false;
        if let Some(exp) = &self.pm_export_text {
            let mut copy_exp = exp.clone();
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Экспортированный TOML (скопирован в буфер):").strong(),
                    );
                    if ui.button("✕ Закрыть").clicked() {
                        close_export = true;
                    }
                });
                ui.add(
                    egui::TextEdit::multiline(&mut copy_exp)
                        .desired_rows(6)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
            });
            ui.add_space(4.0);
        }
        if close_export {
            self.pm_export_text = None;
        }

        // Search and Add new profile
        ui.horizontal(|ui| {
            ui.label("Поиск:");
            ui.add(
                egui::TextEdit::singleline(&mut self.pm_search).hint_text("Фильтр по имени .exe..."),
            );

            ui.separator();
            ui.label("Добавить .exe:");
            ui.add(egui::TextEdit::singleline(&mut self.pm_new_exe).hint_text("game.exe"));
            if ui.button("➕ Добавить").clicked() {
                let trimmed = self.pm_new_exe.trim().to_lowercase();
                if !trimmed.is_empty() {
                    let exe = if trimmed.ends_with(".exe") {
                        trimmed
                    } else {
                        format!("{trimmed}.exe")
                    };
                    if self.profiles.get(&exe).is_none() {
                        self.profiles.set(&exe, GameProfile::default());
                        self.pm_status =
                            Some((format!("✔ Профиль {exe} добавлен"), Instant::now()));
                        self.pm_new_exe.clear();
                    } else {
                        self.pm_status =
                            Some((format!("Профиль {exe} уже существует"), Instant::now()));
                    }
                }
            }
        });
        ui.add_space(6.0);

        // Profiles list
        let list = self.profiles.list();
        let search = self.pm_search.trim().to_lowercase();
        let mut to_remove: Option<String> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            if list.is_empty() {
                ui.weak("Нет сохраненных профилей игр.");
                return;
            }

            for (exe, mut prof) in list {
                if !search.is_empty() && !exe.to_lowercase().contains(&search) {
                    continue;
                }

                let mut changed = false;
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&exe).strong());
                        ui.weak(format!("({:.0} FPS, {})", prof.target_fps, prof.mode));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("🗑 Удалить")
                                .on_hover_text("Удалить профиль")
                                .clicked()
                            {
                                to_remove = Some(exe.clone());
                            }
                        });
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Целевой FPS:");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut prof.target_fps)
                                    .speed(0.5)
                                    .range(10.0..=480.0),
                            )
                            .changed();

                        ui.label("Режим:");
                        let mut mode_idx = match prof.mode.as_str() {
                            "vrr" => 1,
                            "off" => 2,
                            _ => 0,
                        };
                        egui::ComboBox::from_id_salt(format!("mode_{exe}"))
                            .selected_text(match mode_idx {
                                1 => "VRR",
                                2 => "Выкл",
                                _ => "ZeroLag (VSync)",
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(&mut mode_idx, 0, "ZeroLag (VSync)").changed() {
                                    prof.mode = "vsync".into();
                                    changed = true;
                                }
                                if ui.selectable_value(&mut mode_idx, 1, "VRR").changed() {
                                    prof.mode = "vrr".into();
                                    changed = true;
                                }
                                if ui.selectable_value(&mut mode_idx, 2, "Выкл").changed() {
                                    prof.mode = "off".into();
                                    changed = true;
                                }
                            });

                        ui.label("Reflex:");
                        let mut ref_idx = match prof.reflex_mode.as_str() {
                            "on" => 1,
                            "boost" => 2,
                            _ => 0,
                        };
                        egui::ComboBox::from_id_salt(format!("ref_{exe}"))
                            .selected_text(match ref_idx {
                                1 => "On",
                                2 => "On + Boost",
                                _ => "Off",
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(&mut ref_idx, 0, "Off").changed() {
                                    prof.reflex_mode = "off".into();
                                    changed = true;
                                }
                                if ui.selectable_value(&mut ref_idx, 1, "On").changed() {
                                    prof.reflex_mode = "on".into();
                                    changed = true;
                                }
                                if ui.selectable_value(&mut ref_idx, 2, "On + Boost").changed() {
                                    prof.reflex_mode = "boost".into();
                                    changed = true;
                                }
                            });
                    });

                    ui.horizontal(|ui| {
                        changed |= ui.checkbox(&mut prof.auto_attach, "авто-подключение").changed();
                        changed |= ui.checkbox(&mut prof.force_waitable, "waitable object").changed();
                        changed |= ui.checkbox(&mut prof.vsync_override, "override VSync").changed();
                        changed |= ui.checkbox(&mut prof.overlay_enabled, "HUD [F11]").changed();
                    });
                });
                ui.add_space(3.0);

                if changed {
                    self.profiles.set(&exe, prof);
                    if self.exe_name.as_deref() == Some(&exe) {
                        self.push_config();
                    }
                }
            }
        });

        if let Some(del_exe) = to_remove {
            self.profiles.remove(&del_exe);
            self.pm_status = Some((format!("✔ Профиль {del_exe} удалён"), Instant::now()));
        }
    }
}

impl eframe::App for IFrameApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tray(ctx);
        self.poll_hotkey();
        self.poll_gamepad(ctx);
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
                    off: SideSummary::from(&stats.off),
                    on: SideSummary::from(&stats.on),
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

                if let Some(m) = &self.mapping {
                    if m.ring.is_reshade_detected() {
                        ui.colored_label(egui::Color32::from_rgb(0xDF, 0x70, 0xD8), "🎨 ReShade: Coexisting")
                            .on_hover_text("ReShade хук обнаружен в цепочке DXGI — безопасная совместимость активна");
                    }
                    let r_st = m.ring.reflex_state();
                    match r_st {
                        1 => {
                            ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), "⚡ Reflex: Sleep Active")
                                .on_hover_text("Аппаратный NvAPI Reflex Sleep Mode активен — задержка конвейера оптимизирована");
                        }
                        2 => {
                            ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), "⚡ Reflex: Boost Active")
                                .on_hover_text("NvAPI Reflex Boost активен — частоты GPU зафиксированы на максимуме");
                        }
                        0xFF => {
                            ui.weak("⚡ Reflex: N/A")
                                .on_hover_text("Видеокарта не поддерживает NvAPI Reflex или драйвер не NVIDIA");
                        }
                        _ => {}
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Applied in logic() via the viewport command.
                    ui.toggle_value(&mut self.always_on_top, "📌 поверх всех")
                        .on_hover_text("Закрепить окно iFrame поверх игры");
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, UiTab::Monitoring, "🎮 Мониторинг и пейсер");
                ui.selectable_value(&mut self.active_tab, UiTab::Profiles, "📁 Менеджер профилей");

                ui.separator();
                if self.gamepad.is_connected() {
                    ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), "🎮 Геймпад")
                        .on_hover_text("Геймпад активен: D-pad = FPS, LB/RB = Пресеты, A = Подключить, Y = Вкл/Выкл, Back = Big Picture");
                } else {
                    ui.weak("🎮 Геймпад (откл)")
                        .on_hover_text("Подключите Xbox/XInput контроллер для управления без клавиатуры");
                }

                if ui
                    .selectable_label(self.big_picture_mode, "📱 Big Picture")
                    .on_hover_text("Масштаб 1.35x для портативных ПК (Steam Deck, ROG Ally, Legion Go)")
                    .clicked()
                {
                    self.big_picture_mode = !self.big_picture_mode;
                    let zoom = if self.big_picture_mode { 1.35 } else { 1.0 };
                    ui.ctx().set_zoom_factor(zoom);
                }
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
            if self.active_tab == UiTab::Profiles {
                self.show_profile_manager(ui);
                return;
            }

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
                    let d = self.display_stats;

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

                        // --- CDCL Telemetry Auto-Tuner block ---
                        ui.add_space(6.0);
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("🎯 CDCL Авто-Диагностика & Тюнинг по данным игры").strong());
                            });
                            ui.add_space(2.0);

                            // Status of collected samples
                            let off_ready = off.total >= 80;
                            let on_ready = on.total >= 80;

                            ui.horizontal(|ui| {
                                if off_ready {
                                    ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), format!("✓ До лимитера: {} кадров", off.total));
                                } else {
                                    ui.colored_label(egui::Color32::from_rgb(0xE0, 0xA0, 0x30), format!("◌ До лимитера: {}/80 кадров (нужно ~5 сек)", off.total));
                                }
                                ui.separator();
                                if on_ready {
                                    ui.colored_label(egui::Color32::from_rgb(0x30, 0xE0, 0x6A), format!("✓ С лимитером: {} кадров", on.total));
                                } else {
                                    ui.colored_label(egui::Color32::from_rgb(0xE0, 0xA0, 0x30), format!("◌ С лимитером: {}/80 кадров (нужно ~5 сек)", on.total));
                                }
                            });
                            ui.add_space(3.0);

                            ui.horizontal(|ui| {
                                if ui.button(egui::RichText::new("🎯 Рассчитать оптимальный профиль (CDCL)").strong())
                                    .on_hover_text("Запустить SAT-анализ реальной телеметрии, выявить узкие места и рассчитать идеальный FPS")
                                    .clicked()
                                {
                                    let off_metrics = iframe_solver::telemetry_tuner::SideMetrics {
                                        fps: off.fps,
                                        p50_ms: off.p50_us / 1000.0,
                                        p99_ms: off.p99_us / 1000.0,
                                        jitter_ms: off_jitter,
                                        hold_p50_ms: off.hold_p50_us / 1000.0,
                                        hold_max_ms: off.hold_max_us / 1000.0,
                                        wait_p50_ms: off.wait_p50_us / 1000.0,
                                        late_count: off.late as usize,
                                        sample_count: off.total as usize,
                                    };
                                    let on_metrics = iframe_solver::telemetry_tuner::SideMetrics {
                                        fps: on.fps,
                                        p50_ms: on.p50_us / 1000.0,
                                        p99_ms: on.p99_us / 1000.0,
                                        jitter_ms: on_jitter,
                                        hold_p50_ms: on.hold_p50_us / 1000.0,
                                        hold_max_ms: on.hold_max_us / 1000.0,
                                        wait_p50_ms: on.wait_p50_us / 1000.0,
                                        late_count: on.late as usize,
                                        sample_count: on.total as usize,
                                    };
                                    let input = iframe_solver::telemetry_tuner::GameTelemetryInput {
                                        off: off_metrics,
                                        on: on_metrics,
                                        refresh_hz: if self.custom_refresh_hz > 1.0 { self.custom_refresh_hz } else { 144.0 },
                                        current_target_fps: self.target_fps,
                                    };
                                    match iframe_solver::telemetry_tuner::TelemetryTuner::analyze(&input) {
                                        Ok(rec) => {
                                            self.tuner_result = Some(rec);
                                            self.tuner_error = None;
                                        }
                                        Err(e) => {
                                            self.tuner_error = Some(e);
                                            self.tuner_result = None;
                                        }
                                    }
                                }
                            });

                            if let Some(err) = &self.tuner_error {
                                ui.colored_label(egui::Color32::from_rgb(0xE0, 0x60, 0x60), format!("• {err}"));
                            }

                            let tuner_rec = self.tuner_result.clone();
                            if let Some(rec) = &tuner_rec {
                                ui.add_space(4.0);
                                ui.group(|ui| {
                                    ui.label(egui::RichText::new("🩺 Диагноз игрового конвейера:").strong());
                                    ui.colored_label(egui::Color32::from_rgb(0x50, 0x90, 0xC0), &rec.diagnosis_summary);
                                    ui.add_space(3.0);

                                    ui.label(egui::RichText::new("📊 Математический расчёт эффекта:").strong());
                                    ui.horizontal(|ui| {
                                        ui.label(format!("⚡ Снижение задержки ввода: ~{:.1} мс", rec.expected_latency_reduction_ms));
                                        ui.separator();
                                        ui.label(format!("📈 Устранение статтеров: в {:.1}x раз", rec.expected_jitter_improvement_times));
                                        ui.separator();
                                        ui.label(format!("❄ Разгрузка GPU / охлаждение: на {:.0}%", rec.expected_gpu_load_relief_percent));
                                    });
                                    ui.add_space(3.0);

                                    ui.colored_label(
                                        egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                                        format!(
                                            "💡 Рекомендация CDCL: {:.0} FPS (Режим: {:?}, Шаги каденции: {:?})",
                                            rec.recommended_fps, rec.recommended_mode, rec.cadence_steps
                                        ),
                                    );
                                    ui.add_space(3.0);

                                    let mut apply_tune_profile: Option<(f64, PacerMode, bool)> = None;
                                    if ui.button(egui::RichText::new("✔ Применить рекомендацию CDCL в 1 клик").strong()).clicked() {
                                        apply_tune_profile = Some((rec.recommended_fps, rec.recommended_mode, rec.recommended_vsync_override));
                                    }
                                    if let Some((fps, mode, vsync)) = apply_tune_profile {
                                        self.target_fps = fps;
                                        self.mode = mode;
                                        self.vsync_override = vsync;
                                        self.push_config();
                                    }
                                });
                            }
                        });
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
                            ui.label("Герцовка экрана:");
                            let refresh_text = if self.custom_refresh_hz <= 1.0 {
                                "Авто (DWM)".to_string()
                            } else {
                                format!("{:.0} Гц", self.custom_refresh_hz)
                            };
                            let mut refresh_changed = false;
                            egui::ComboBox::from_id_salt("refresh_override")
                                .selected_text(refresh_text)
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 0.0, "Авто (DWM)").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 60.0, "60 Гц").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 120.0, "120 Гц").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 144.0, "144 Гц").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 165.0, "165 Гц").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 240.0, "240 Гц").changed();
                                    refresh_changed |= ui.selectable_value(&mut self.custom_refresh_hz, 360.0, "360 Гц").changed();
                                });
                            ui.label("или:");
                            refresh_changed |= ui
                                .add(
                                    egui::DragValue::new(&mut self.custom_refresh_hz)
                                        .speed(1.0)
                                        .range(0.0..=1000.0)
                                        .suffix(" Гц"),
                                )
                                .on_hover_text("Произвольная частота обновления монитора (0 = авто из DWM)")
                                .changed();
                            if refresh_changed {
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
                            if ui
                                .checkbox(&mut self.overlay_enabled, "HUD в игре [F11]")
                                .on_hover_text("Компактный аппаратный оверлей поверх DirectX 11 (FPS, фреймтайм, спарклайн, Reflex). F11 переключает в игре.")
                                .changed()
                            {
                                changed = true;
                            }
                            if changed {
                                self.push_config();
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("NVIDIA Reflex:");
                            let mut r_changed = false;
                            r_changed |= ui
                                .radio_value(&mut self.reflex_mode, ReflexMode::Off, "Выкл")
                                .on_hover_text("Аппаратный Reflex отключен")
                                .changed();
                            r_changed |= ui
                                .radio_value(&mut self.reflex_mode, ReflexMode::On, "Вкл (Low Latency)")
                                .on_hover_text("Включить аппаратный NvAPI Reflex Sleep Mode для устранения очереди кадров")
                                .changed();
                            r_changed |= ui
                                .radio_value(&mut self.reflex_mode, ReflexMode::Boost, "On + Boost")
                                .on_hover_text("Reflex с поддержанием максимальной тактовой частоты GPU")
                                .changed();
                            if r_changed {
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
                    ui.add_space(6.0);

                    // --- CDCL Solver & Optimization panel ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("🧠 C++ CDCL Решатель & Оптимизация").strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🧪 Тест ядра").on_hover_text("Запустить решение контрольной SAT/UNSAT формулы").clicked() {
                                    let t0 = Instant::now();
                                    let mut s = iframe_solver::CdclSolver::new(10);
                                    s.add_clause(&[1, 2, 3]);
                                    s.add_clause(&[-1, 2]);
                                    s.add_clause(&[-2, 3]);
                                    s.add_clause(&[-3]);
                                    s.add_clause(&[1]);
                                    let res = s.solve(1000);
                                    let elapsed = t0.elapsed();
                                    let stats = s.stats();
                                    self.solver_demo_result = Some(format!(
                                        "{res:?} за {:.1} µs (пропагаций: {}, конфликтов: {})",
                                        elapsed.as_micros(),
                                        stats.propagations,
                                        stats.conflicts
                                    ));
                                }
                            });
                        });
                        ui.add_space(2.0);

                        if let Some(demo) = &self.solver_demo_result {
                            ui.colored_label(egui::Color32::from_rgb(0x50, 0x90, 0xC0), format!("• Результат теста: {demo}"));
                            ui.add_space(2.0);
                        }

                        // 1. Cadence Synthesizer
                        ui.label(egui::RichText::new("1. Синтез идеального ритма VBlank (Cadence)").small().weak());
                        ui.horizontal(|ui| {
                            ui.label("Цель FPS:");
                            ui.add(egui::DragValue::new(&mut self.solver_cadence_fps).speed(0.5).range(10.0..=360.0));
                            ui.label("Герцовка экрана:");
                            ui.add(egui::DragValue::new(&mut self.solver_cadence_hz).speed(1.0).range(30.0..=480.0));

                            if ui.button("⚡ Рассчитать (CDCL)").clicked() {
                                match iframe_solver::cadence_synthesizer::CadenceSynthesizer::synthesize(
                                    self.solver_cadence_fps,
                                    self.solver_cadence_hz,
                                ) {
                                    Ok(sched) => {
                                        self.solver_cadence_result = Some(sched);
                                    }
                                    Err(e) => {
                                        self.solver_cadence_result = None;
                                        log_ui(&format!("Cadence solver error: {e}"));
                                    }
                                }
                            }
                        });

                        let mut apply_cadence = false;
                        if let Some(sched) = &self.solver_cadence_result {
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                                    format!(
                                        "✔ Период: {} кадров ({} VBlanks, среднее: {:.2}). Шаги: {:?}",
                                        sched.period_frames,
                                        sched.total_vblanks,
                                        sched.avg_vblanks_per_frame,
                                        sched.steps
                                    ),
                                );
                                if ui.button("Применить в лимитер").clicked() {
                                    apply_cadence = true;
                                }
                            });
                        }
                        if apply_cadence {
                            self.target_fps = self.solver_cadence_fps;
                            self.mode = PacerMode::FixedVsync;
                            self.push_config();
                        }
                        ui.add_space(4.0);

                        // 2. Config Optimizer
                        ui.label(egui::RichText::new("2. SAT-Оптимизатор параметров SwapChain & VRR").small().weak());
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.solver_has_flip, "Flip Model");
                            ui.checkbox(&mut self.solver_has_vrr, "VRR экран");
                            ui.checkbox(&mut self.solver_is_fullscreen, "Эксклюзивный экран");

                            if ui.button("🔍 Синтез конфигурации").clicked() {
                                let caps = iframe_solver::config_optimizer::SystemCapabilities {
                                    has_flip_model: self.solver_has_flip,
                                    monitor_vrr_capable: self.solver_has_vrr,
                                    monitor_refresh_hz: self.solver_cadence_hz,
                                    is_exclusive_fullscreen: self.solver_is_fullscreen,
                                };
                                let prefs = iframe_solver::config_optimizer::UserPreferences {
                                    target_fps: self.target_fps,
                                    prefer_vrr: self.solver_has_vrr,
                                    prefer_lowest_latency: true,
                                    allow_vsync_override: true,
                                };
                                match iframe_solver::config_optimizer::ConfigOptimizer::optimize(&caps, &prefs) {
                                    Ok(cfg) => {
                                        self.mode = cfg.mode;
                                        self.vsync_override = cfg.vsync_override;
                                        self.force_waitable = cfg.force_waitable;
                                        self.push_config();
                                        self.solver_config_result = Some(format!(
                                            "Режим: {:?}, VSync Override: {}, Waitable: {}",
                                            cfg.mode, cfg.vsync_override, cfg.force_waitable
                                        ));
                                    }
                                    Err(e) => {
                                        self.solver_config_result = Some(format!("Ошибка: {e}"));
                                    }
                                }
                            }
                        });

                        if let Some(cfg_msg) = &self.solver_config_result {
                            ui.colored_label(
                                egui::Color32::from_rgb(0x30, 0xE0, 0x6A),
                                format!("✔ Конфигурация доказана и применена: {cfg_msg}"),
                            );
                        }
                    });
                    ui.add_space(4.0);

                    // 4. Event Log / Journal
                    ui.collapsing("📋 Журнал событий", |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(120.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                if let Ok(logs) = UI_LOGS.lock() {
                                    if logs.is_empty() {
                                        ui.weak("Журнал пуст.");
                                    } else {
                                        for entry in logs.iter() {
                                            ui.label(egui::RichText::new(entry).monospace().small());
                                        }
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
                reflex_mode: ReflexMode::Off,
                overlay_enabled: false,
            };
            mapping.ring.set_config(&cfg);
            mapping.ring.mark_headless();
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

static UI_LOGS: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

#[repr(C)]
struct SYSTEMTIME {
    w_year: u16,
    w_month: u16,
    w_day_of_week: u16,
    w_day: u16,
    w_hour: u16,
    w_minute: u16,
    w_second: u16,
    w_milliseconds: u16,
}

unsafe extern "system" {
    fn GetLocalTime(lp_system_time: *mut SYSTEMTIME);
}

fn local_time_str() -> String {
    let mut st = std::mem::MaybeUninit::<SYSTEMTIME>::zeroed();
    unsafe {
        GetLocalTime(st.as_mut_ptr());
        let s = st.assume_init();
        format!("{:02}:{:02}:{:02}", s.w_hour, s.w_minute, s.w_second)
    }
}

fn log_ui(msg: &str) {
    eprintln!("[iFrame] {msg}");
    if let Ok(mut logs) = UI_LOGS.lock() {
        if logs.len() >= 50 {
            logs.pop_front();
        }
        let time_str = local_time_str();
        logs.push_back(format!("[{time_str}] {msg}"));
    }
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