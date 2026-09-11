//! All In Frame Control Center & Optical Flow Frame Generation Engine.

mod injector;

use std::sync::atomic::Ordering;
use std::time::Instant;

use aif_presenter::PresenterController;
use eframe::egui;
use injector::{list_running_game_windows, ProcessInfo};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_F11, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

#[derive(PartialEq, Clone, Copy)]
enum TargetMode {
    AutoForeground,
    ManualSelection,
}

struct AllInFrameApp {
    presenter: PresenterController,
    processes: Vec<ProcessInfo>,
    selected_pid: Option<u32>,
    status_message: String,
    last_process_refresh: Instant,
    fps_multiplier: u32,
    debug_flow: bool,
    sharpening: f32,
    target_mode: TargetMode,
    search_query: String,
    minimize_to_tray: bool,
    is_minimized: bool,
    hotkey_prev: bool,
    restore_hotkey_prev: bool,
}

impl AllInFrameApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            presenter: PresenterController::new(),
            processes: Vec::new(),
            selected_pid: None,
            status_message: "Готов к запуску. Нажмите F11 в любой игре для мгновенного удвоения кадров.".to_string(),
            last_process_refresh: Instant::now(),
            fps_multiplier: 2,
            debug_flow: false,
            sharpening: 0.50,
            target_mode: TargetMode::AutoForeground,
            search_query: String::new(),
            minimize_to_tray: true,
            is_minimized: false,
            hotkey_prev: false,
            restore_hotkey_prev: false,
        };
        app.refresh_processes();
        app
    }

    fn refresh_processes(&mut self) {
        self.processes = list_running_game_windows();
        if self.selected_pid.is_none() && !self.processes.is_empty() {
            self.selected_pid = Some(self.processes[0].pid);
        }
        self.last_process_refresh = Instant::now();
    }

    fn resolve_target_hwnd(&self) -> Option<HWND> {
        if self.target_mode == TargetMode::AutoForeground {
            let fg = unsafe { GetForegroundWindow() };
            if !fg.0.is_null() {
                let mut title_buf = [0u16; 256];
                let len = unsafe { GetWindowTextW(fg, &mut title_buf) };
                if len > 0 {
                    let title = String::from_utf16_lossy(&title_buf[..len as usize]);
                    if !title.contains("All In Frame") && title != "Program Manager" && title != "Default IME" {
                        return Some(fg);
                    }
                }
            }
        }

        self.selected_pid
            .and_then(|pid| self.processes.iter().find(|p| p.pid == pid))
            .map(|p| HWND(p.hwnd as *mut _))
    }

    fn toggle_frame_generation(&mut self) {
        if self.presenter.is_running() {
            self.presenter.stop();
            self.status_message = "⏹ Генерация кадров остановлена.".to_string();
        } else {
            let hwnd = match self.resolve_target_hwnd() {
                Some(h) => h,
                None => {
                    self.status_message = "❌ Ошибка: целевое окно игры не найдено. Выберите окно из списка.".to_string();
                    return;
                }
            };

            match self.presenter.start(
                hwnd,
                self.fps_multiplier,
                self.debug_flow,
                self.sharpening,
            ) {
                Ok(_) => {
                    self.status_message = format!(
                        "⚡ Генерация АКТИВНА ({multiplier}x) | FidelityFX CAS: {sharp:.0}% | F12: HUD",
                        multiplier = self.fps_multiplier,
                        sharp = self.sharpening * 100.0,
                    );
                }
                Err(err) => {
                    self.status_message = format!("❌ Ошибка запуска генератора: {err}");
                }
            }
        }
    }
}

impl eframe::App for AllInFrameApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Continuous repaint for responsive live telemetry
        ctx.request_repaint_after(std::time::Duration::from_millis(35));

        if self.last_process_refresh.elapsed().as_secs() >= 3 {
            self.refresh_processes();
        }

        // Global Hotkeys:
        let ctrl_down = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
        let alt_down = unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } < 0;
        let f_down = unsafe { GetAsyncKeyState(0x46) } < 0; // 'F'
        let a_down = unsafe { GetAsyncKeyState(0x41) } < 0; // 'A'
        let f11_down = unsafe { GetAsyncKeyState(VK_F11.0 as i32) } < 0;
        let hotkey_active = (ctrl_down && alt_down && f_down) || f11_down;

        if hotkey_active && !self.hotkey_prev {
            self.toggle_frame_generation();
        }
        self.hotkey_prev = hotkey_active;

        // Restore window on Ctrl + Alt + A
        let restore_active = ctrl_down && alt_down && a_down;
        if restore_active && !self.restore_hotkey_prev {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.is_minimized = false;
        }
        self.restore_hotkey_prev = restore_active;

        // Handle Close button -> Minimize to tray
        if ctx.input(|i| i.viewport().close_requested()) && self.minimize_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.is_minimized = true;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);

            // Header Banner
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new("⚡ All In Frame")
                        .size(24.0)
                        .color(egui::Color32::from_rgb(0, 220, 160))
                        .strong(),
                );
                ui.label(
                    egui::RichText::new("Optical Flow Frame Generator & Lossless Scaling")
                        .size(13.0)
                        .italics()
                        .color(egui::Color32::LIGHT_GRAY),
                );
            });
            ui.separator();

            // Real Hardware Telemetry Card
            let is_running = self.presenter.is_running();
            let game_fps = self.presenter.telemetry().game_fps();
            let out_fps = self.presenter.telemetry().output_fps();
            let captured_count = self.presenter.telemetry().frames_captured.load(Ordering::Relaxed);
            let presented_count = self.presenter.telemetry().frames_presented.load(Ordering::Relaxed);

            ui.group(|ui| {
                ui.label(egui::RichText::new("📊 Аппаратная телеметрия в реальном времени").strong());
                ui.horizontal(|ui| {
                    if is_running {
                        ui.colored_label(egui::Color32::from_rgb(0, 230, 120), "🟢 Генератор: АКТИВЕН");
                    } else {
                        ui.colored_label(egui::Color32::GRAY, "⚪ Генератор: ОЖИДАНИЕ");
                    }

                    ui.separator();

                    ui.colored_label(
                        egui::Color32::from_rgb(0, 200, 255),
                        "🎯 Поток: Coarse-to-Fine + Disocclusion",
                    );

                    ui.separator();

                    ui.colored_label(
                        egui::Color32::LIGHT_BLUE,
                        format!("Кадров: {captured_count} захвачено / {presented_count} выведено"),
                    );
                });

                if is_running && game_fps > 0.5 {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 200, 210),
                            format!("🎮 FPS Игры (честный захват): {:.1} FPS", game_fps),
                        );
                        ui.separator();
                        let mult = self.fps_multiplier;
                        let (color, tag) = if mult == 1 {
                            (egui::Color32::from_rgb(0, 220, 160), "1x Pass-through")
                        } else if mult == 2 {
                            (egui::Color32::from_rgb(0, 200, 255), "2x Удвоение")
                        } else {
                            (egui::Color32::from_rgb(220, 120, 255), "3x Утроение")
                        };
                        ui.colored_label(
                            color,
                            format!("🚀 Вывод на монитор: {:.1} FPS ({tag})", out_fps),
                        );
                    });
                }
            });

            ui.add_space(2.0);

            // Target Selection Card
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🎯 Режим выбора целевой игры:").strong());
                    ui.selectable_value(&mut self.target_mode, TargetMode::AutoForeground, "⚡ Автозахват (Foreground)");
                    ui.selectable_value(&mut self.target_mode, TargetMode::ManualSelection, "📌 Ручной выбор окна");
                });

                if self.target_mode == TargetMode::AutoForeground {
                    ui.label(
                        egui::RichText::new("✨ Режим автозахвата: просто откройте любую игру и нажмите F11 — генератор зацепит её мгновенно.")
                            .size(12.0)
                            .color(egui::Color32::from_rgb(180, 220, 255))
                    );
                } else {
                    // Manual Selection View with Search & Process List
                    ui.horizontal(|ui| {
                        ui.label("🔎 Поиск:");
                        ui.text_edit_singleline(&mut self.search_query);
                        if ui.button("🔄 Обновить список").clicked() {
                            self.refresh_processes();
                        }
                    });

                    let query_lower = self.search_query.to_lowercase();
                    let filtered_processes: Vec<_> = self.processes.iter().filter(|p| {
                        query_lower.is_empty()
                            || p.name.to_lowercase().contains(&query_lower)
                            || p.window_title.to_lowercase().contains(&query_lower)
                    }).collect();

                    egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                        for p in filtered_processes {
                            let is_selected = self.selected_pid == Some(p.pid);
                            let title = format!("🎮 {} (PID: {}) — {}", p.name, p.pid, p.window_title);
                            if ui.selectable_label(is_selected, title).clicked() {
                                self.selected_pid = Some(p.pid);
                            }
                        }
                    });

                    if let Some(pid) = self.selected_pid {
                        if let Some(p) = self.processes.iter().find(|pr| pr.pid == pid) {
                            ui.colored_label(
                                egui::Color32::from_rgb(0, 230, 160),
                                format!("🔒 Зафиксировано на: {} ({})", p.name, p.window_title),
                            );
                        }
                    }
                }

                ui.add_space(4.0);

                // Big Primary Action Button
                if is_running {
                    let btn = egui::Button::new(
                        egui::RichText::new("⏹ ОСТАНОВИТЬ ГЕНЕРАЦИЮ КАДРОВ (F11)")
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(200, 50, 50))
                    .min_size(egui::vec2(ui.available_width(), 42.0));

                    if ui.add(btn).clicked() {
                        self.toggle_frame_generation();
                    }
                } else {
                    let btn = egui::Button::new(
                        egui::RichText::new("🚀 ВКЛЮЧИТЬ ГЕНЕРАЦИЮ КАДРОВ (F11)")
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::BLACK),
                    )
                    .fill(egui::Color32::from_rgb(0, 220, 160))
                    .min_size(egui::vec2(ui.available_width(), 42.0));

                    if ui.add(btn).clicked() {
                        self.toggle_frame_generation();
                    }
                }

                ui.label(egui::RichText::new(&self.status_message).size(12.0).color(egui::Color32::KHAKI));
            });

            ui.add_space(2.0);

            // Frame Generation & Post-Processing Settings Card
            ui.group(|ui| {
                ui.label(egui::RichText::new("⚙️ Настройки синтеза и качества (FidelityFX CAS & Опции)").strong());
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.fps_multiplier, 1, "1x (Pass-through)");
                    ui.selectable_value(&mut self.fps_multiplier, 2, "2x (Удвоение 60 -> 120 FPS)");
                    ui.selectable_value(&mut self.fps_multiplier, 3, "3x (Утроение 30 -> 90 FPS)");
                });

                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("🔍 Резкость AMD FidelityFX CAS:");
                    ui.add(egui::Slider::new(&mut self.sharpening, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
                });

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.minimize_to_tray, "📌 Сворачивать в системный трей при закрытии (Ctrl + Alt + A для возврата)");
                    ui.checkbox(&mut self.debug_flow, "🎨 Debug Motion");
                });

                ui.label(
                    egui::RichText::new("💡 Горячие клавиши: F11 — Вкл/Выкл генерации | F12 — Внутриигровой HUD фреймтайма | Ctrl+Alt+A — Восстановить окно")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(180, 220, 255))
                );
            });
        });
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 580.0])
            .with_min_inner_size([540.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "All In Frame — Optical Flow Frame Generator",
        options,
        Box::new(|cc| Ok(Box::new(AllInFrameApp::new(cc)))),
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}
