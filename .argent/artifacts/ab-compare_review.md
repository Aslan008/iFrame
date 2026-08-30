# Review: ab-compare (A/B-сравнение фреймтайма OFF vs ON)

## As-built
В UI под строкой статистики появилась таблица: FPS / p50 / p99 отдельно для периодов с выключенным («limiter OFF (before)») и включённым («limiter ON (after)») лимитером. Каждая сторона — собственное скользящее окно 10 с; неактивная сторона «замораживается» на хвосте своего периода (не исчезает со временем). Пустая сторона (кадров < 2) — «—». Заголовок: «frametime, last 10 s per state».

Данные: воркер `live.rs` раскладывает каждый кадр в один из двух бакетов по `SharedState.limiter_on` (гранулярность батча 20 мс). Перцентили считаются по всему окну бакета (не за 1 с, как заголовочная строка). Хук, протокол shared memory и CLI не тронуты.

## Изменено vs одобренная спека (из журнала ab-compare_notes.md)
1. **Верхняя плашка**: исправлен существующий баг — p50/p99 (µs) печатались с меткой «ms» без деления на 1000 (по требованию вердикта п.3.1).
2. **ETW-режим**: колбэк etw.rs теперь публикует `stats.off` (лимитер в ETW невозможен → все кадры = OFF) и заполняет заголовочные FPS/p50/p99, которые в ETW всегда были 0.0 (вердикт п.3.2). Троттлинг пересчёта — 20 мс.
3. **Дедуп**: `percentile` вынесен в `live.rs` (pub(crate)), копия из watch.rs удалена.

## Файлы
- `crates/iframe-app/src/live.rs` — `SideStats`, поля `LiveStats.on/off`, `compute_side_stats()`, `SideBucket` (пуш/трим/публикация), бакетизация в воркере, 4 unit-теста.
- `crates/iframe-app/src/ui.rs` — фикс µs→ms в верхней плашке, блок `egui::Grid` A/B, хелпер `ab_val`.
- `crates/iframe-app/src/etw.rs` — троттлинг 20 мс + публикация off/заголовочной статистики в ETW-режиме.
- `crates/iframe-app/src/watch.rs` — импорт `live::percentile` вместо локальной копии.

## Верификация (фактический вывод)
- `cargo test -p iframe-app` → **4 passed; 0 failed** (side_stats_empty_and_single_are_zero, side_stats_constant_frametime_is_flat, side_stats_p99_catches_outliers, side_bucket_trims_to_window_and_freezes_when_inactive).
- `cargo check --workspace --all-targets` → **exit 0**.

## Как проверить руками
1. `cargo build --release` → запустить `target/release/iframe.exe`.
2. Приатачиться к стенду (`tests/d3d11-test-app`): OFF-колонка заполняется естественным FPS.
3. Включить лимитер (например, 40 FPS) → ON-колонка заполняется (~40 FPS, p50 ≈ 25 ms), OFF-колонка замирает на последних 10 с «до».
4. Ctrl+Alt+I — переключение туда-обратно обновляет соответствующие колонки.

## Открытые TODO
- Раскраска графика по состояниям лимитера (сейчас только таблица).
- Напоминание: это сравнение ровности (фреймтайм), не задержки present→экран — та остаётся = 0 по архитектуре и не измеряется.