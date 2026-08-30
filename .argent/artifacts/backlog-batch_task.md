# Задача: backlog-batch — реализация бэклога анализа

Порядок: A (баги 1–6) → B (гигиена 7–8, график 10) → C (панель задержек 12-lite) → D (force_waitable 13) → отложенные 9/11.

## Чек-лист
- [ ] A1. anticheat.rs: убрать "netsh.exe"
- [ ] A2. vsync_override: RuntimeConfig → shared header → override_sync (чекбокс UI становится рабочим)
- [ ] A2b. force_waitable: поля в RuntimeConfig/shared header (провязка хука — этап D)
- [ ] A3. cmd_inject: mapping живёт до конца инъекции (убрать гонку)
- [ ] A4. --refresh: override refresh_hz в engine::pace
- [ ] A5. attach в UI — фоновый поток + poll_attach (без фриза UI)
- [ ] A6. profiles: дебаунс записи TOML (≤1 записи/с) + flush на выходе
- [ ] B7. warnings: allow(unsafe_op_in_unsafe_fn) в хуке + мёртвый код/импорты
- [ ] B8. ETW: Ctrl+C через SetConsoleCtrlHandler, watch.stop() достижим
- [ ] B10. график: две линии по состояниям лимитера (OFF серо-синяя, ON зелёная)
- [ ] C12. панель задержек: p50 «Present call» (hold) и «next-frame delay» (wait) из TelemetryFrame
- [ ] D13. force_waitable_object: UI-чекбокс + форс флага в CreateSwapChain* + ResizeBuffers
- [ ] Верификация: cargo test, cargo check, живой прогон стенд+инъекция+лимит
- [ ] Отложено: 9 (vblank-иерархия D3DKMT — нужно живое железо), 11 (D3D9 inline-хуки — исследовательская задача)

## Отложенные (обоснование)
- **9**: D3DKMTGetScanLine/WaitForVerticalBlankEvent требуют проверки на реальных мониторах (фаза/дрейф); слепая реализация может сломать работающий DWM-путь.
- **11**: inline-трамплины экспортов d3d9.dll — исследовательская работа с высоким риском крашей; d3d9.dll динамически строит vtable и откатывает патчи (доказано в M5).