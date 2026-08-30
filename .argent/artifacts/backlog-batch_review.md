# Review: backlog-batch (реализация бэклога анализа)

## As-built — что теперь существует
Все пункты 1–8, 10, 12-lite и 13 из бэклога реализованы. Ядро (пейсер, DXGI-хуки, IPC) не переписывалось — изменения конфиг-протокола обратно совместимы по размеру заголовка (align(64) → прежние 128 байт).

**Новые возможности для пользователя:**
- Чекбокс **«override VSync»** теперь реально управляет К1-override в хуке (раньше был no-op).
- Чекбокс **«waitable object»** — per-game opt-in: хук форсирует `FRAME_LATENCY_WAITABLE_OBJECT` на новых свопчейнах игры (CreateSwapChain/ForHwnd/ForCoreWindow/ForComposition) и добавляет флаг в игру's `ResizeBuffers` (К5), иначе игра получила бы `DXGI_ERROR_INVALID_CALL`.
- **A/B-таблица дополнена строками задержек**: «Present call» — p50 длительности реального Present (не меняется от лимитера → доказательство «кадр не удерживается»), «next-frame delay» — p50 ожидания пейсера после Present (задерживается только старт следующего кадра). В ETW-режиме эти строки — «—» (нет данных о длительности Present).
- **График**: две линии — серо-синяя (лимитер OFF) и зелёная (ON) — переход виден прямо на графике.
- CLI: `iframe limit --pid N --fps F [--no-vsync-override] [--waitable]`; `--refresh` теперь работает (override герцовки, фаза остаётся на DWM-сетке).
- UI: attach выполняется в фоновом потоке (статус «◌ attaching…»), окно больше не фризится до 5 с; профили пишутся на диск не чаще 1 раза/с + flush при выходе.
- `watch-etw`: корректная остановка по Ctrl+C (SetConsoleCtrlHandler, фича Win32_System_Console), трасса останавливается чисто.

## Изменено vs первоначальный план (журнал: backlog-batch_task.md)
1. **Живой прогон (стенд+инъекция+лимит+waitable) прерван пользователем** — не выполнялся; вместо него — статическая верификация + готовый ручной сценарий (ниже). Для него в стенд добавлен `--delay N` (создание свопчейна после публикации конфига).
2. **unsafe_op_in_unsafe_fn (89 шт.)** — не расставлялись явные `unsafe {}`-блоки (риск механических ошибок в 90+ местах горячего пути); выбран crate-level `#![allow]` с обоснованием в коде. Отдельная механическая задача в бэклоге.
3. **Пункт 12** (present→экран через ETW, PresentMon-подход) реализован как SM-телеметрическая панель (hold/wait p50) — честная метрика «нулевой добавки» из уже имеющихся данных; полный ETW-подсчёт скан-аута остался в бэклоге.

## Файлы
- `crates/iframe-common/src/config.rs` — RuntimeConfig: +vsync_override, +force_waitable
- `crates/iframe-common/src/shared_mem.rs` — SharedHeader: +2 атомика (размер заголовка не изменился), set_config/config
- `crates/iframe-hook/src/engine.rs` — user-refresh override hint'а
- `crates/iframe-hook/src/hooks/dxgi.rs` — override_sync(vsync_override), force_waitable_desc/desc1 + форс в 4 create-хуках + ResizeBuffers (К5)
- `crates/iframe-hook/src/lib.rs` — allow(unsafe_op_in_unsafe_fn) с комментарием
- `crates/iframe-app/src/main.rs` — cmd_inject (mapping жив), cmd_limit (+2 флага), watch-etw Ctrl+C, чистка
- `crates/iframe-app/src/ui.rs` — фоновый attach + poll_attach + статусы, A/B-таблица с задержками, двухцветный график, чекбокс waitable, чистка
- `crates/iframe-app/src/profiles.rs` — дебаунс, +force_waitable
- `crates/iframe-app/src/live.rs` — SideStats(+hold/wait/has_timing), SideBucket(+holds/waits), тесты
- `crates/iframe-app/src/etw.rs` — has_timing=false, чистка
- `crates/iframe-app/src/{injector,sm_host,watch}.rs`, `Cargo.toml` (+Win32_System_Console), тест-стенды — чистка
- `tests/d3d11-test-app/src/main.rs` — опция `--delay N`

## Верификация (фактический вывод)
- `cargo test --workspace` → **13 passed (common) + 4 passed (app), 0 failed**
- `cargo check --workspace --all-targets` → **0 errors, 0 warnings** (было ~110)
- `cargo build --release` → `Finished release profile [optimized] in 44.52s`
- Живой прогон — прерван пользователем, **не выполнялся**.

## Как проверить руками (5 минут)
1. `target/release/d3d11_test_app.exe --delay 6 --tearing --seconds 30`
2. `target/release/iframe.exe inject --window "iFrame Test D3D11"` (сразу, пока свопчейна нет)
3. `target/release/iframe.exe limit --pid <PID из вывода стенда> --fps 40 --waitable`
4. Через ~7 с в `%TEMP%\iframe_hook.log` должно появиться `swapchain created: ... waitable=true` — форс флага сработал.
5. `target/release/iframe.exe watch --pid <PID> --seconds 5` → ~40 FPS.
6. GUI: attach к стенду → включить лимитер → в A/B-таблице OFF-сторона замрёт, ON покажет 40 FPS; строка «Present call» должна остаться ~одинаковой в обеих колонках, «next-frame delay» появится только в ON.

## Открытые TODO
- Расставить явные `unsafe {}`-блоки в iframe-hook (сейчас allow; станет hard error в будущих редакциях Rust).
- Полный подсчёт display-latency через ETW (PresentMon-подход).
- vblank-иерархия (D3DKMT) — нужно живое железо; D3D9 inline-хуки — исследовательская задача.
- Изменения не закоммичены в git (включая правки, бывшие незакоммиченными до этой сессии).