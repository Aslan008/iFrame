# Задача: backlog-batch — реализация бэклога анализа

Порядок: A (баги 1–6) → B (гигиена 7–8, график 10) → C (панель задержек 12-lite) → D (force_waitable 13) → отложенные 9/11.

## Чек-лист
- [x] A1. anticheat.rs: убрать "netsh.exe"
- [x] A2. vsync_override: RuntimeConfig → shared header → override_sync (чекбокс UI стал рабочим)
- [x] A2b. force_waitable: поля в RuntimeConfig/shared header
- [x] A3. cmd_inject: mapping живёт до конца инъекции (гонка устранена)
- [x] A4. --refresh: override refresh_hz в engine::pace (фаза остаётся на DWM-сетке)
- [x] A5. attach в UI — фоновый поток + poll_attach + статус «◌ attaching…» (фриз устранён)
- [x] A6. profiles: дебаунс TOML (≤1 записи/с) + flush на выходе
- [x] B7. warnings: 110 → 0 (allow(unsafe_op_in_unsafe_fn) в хуке + мёртвый код/импорты)
- [x] B8. ETW: Ctrl+C через SetConsoleCtrlHandler, watch.stop() достижим, фича Win32_System_Console
- [x] B10. график: две линии по состояниям лимитера (OFF серо-синяя, ON зелёная)
- [x] C12. панель задержек: строки «Present call» (hold p50) и «next-frame delay» (wait p50); ETW → has_timing=false
- [x] D13. force_waitable_object: чекбокс UI + профиль + форс флага в CreateSwapChain*/ResizeBuffers (К5)
- [x] Верификация: cargo test 17/17, cargo check 0 warnings/0 errors, release-сборка OK
- [ ] Живой прогон стенд+инъекция+лимит — ПРЕРВАН пользователем (ручные шаги в review)
- [x] Стенд: d3d11_test_app --delay N (создание свопчейна после публикации конфига)
- [ ] Отложено: 9 (vblank-иерархия D3DKMT — нужно живое железо), 11 (D3D9 inline-хуки — исследовательская задача)

## Отложенные (обоснование)
- **9**: D3DKMTGetScanLine/WaitForVerticalBlankEvent требуют проверки на реальных мониторах (фаза/дрейф); слепая реализация может сломать работающий DWM-путь.
- **11**: inline-трамплины экспортов d3d9.dll — исследовательская работа с высоким риском крашей; d3d9.dll динамически строит vtable и откатывает патчи (доказано в M5).