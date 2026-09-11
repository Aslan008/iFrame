# Ревью новых реализаций и доработок iFrame — итог

Дата: 2026-09-02. Задача: проверить незакоммиченные доработки (heartbeat-watchdog, refresh-override,
UI-журнал, CDCL-тюнер, SetMaximumFrameLatency, bench-вердикты, фикс пустого заголовка) и дореализовать
найденные пробелы. Метод: сверка каждого слота vtable с сгенерированными структурами windows-0.61.3,
чтение всех диффов, правки, полный тест-прогон, живой E2E.

## Вердикт по присланному коду

Реализовано хорошо: heartbeat-watchdog (дизайн с host_present=0 для CLI — правильный),
refresh-override в UI, журнал событий в UI, честные вердикты bench, фикс пустого заголовка,
CDCL-тюнер (CLI `tune` + UI-панель). НО найдено 3 серьёзных дефекта в новой DXGI-части + 1 логический
пробел в watchdog'е. Все исправлены в этой сессии.

## 🔴 Найдено и исправлено

### 1. Present1 хукнулся в чужой слот (критично, pre-existing)
`SLOT_PRESENT1 = 21` — на самом деле это **GetCoreWindow**; настоящий Present1 = **22**.
Старый подсчёт потерял `ResizeTarget(14)` в базовом IDXGISwapChain (10 методов, не 9).
Последствия: игры на Present1 (современный flip-model) не хукались вовсе; UWP-путь звал хук с чужой сигнатурой.

### 2. Все factory-слоты сдвинуты на 1 (критично, pre-existing)
Старый подсчёт потерял `IsCurrent(13)` в IDXGIFactory1:
- FOR_HWND 14→**15** (14 = IsWindowedStereoEnabled — игры, зовущие её, получали хук создания с мусорными аргументами),
- FOR_CORE_WINDOW 15→**16** (по старому слоту 15 хук core-window с 6 аргументами звался вместо 7-аргументного ForHwnd → создание свапчейна писало результат в чужой out-указатель = крах современных игр),
- FOR_COMPOSITION 23→**24** (23 = UnregisterOcclusionStatus).

### 3. SetMaximumFrameLatency молча не работал (новый код)
Ручной GUID `0xa8be2ac4-b9f0-4c52-...` не совпадает с настоящим IID
(`0xa8be2ac4-199f-4946-b331-79599fb98de7`) → QueryInterface всегда падал → вызов никогда не происходил.
Сам слот 31 был верный. Переписано на типизированный `cast::<IDXGISwapChain2>()` (IID и слот из крейта,
ошибиться невозможно) + попытка один раз на свапчейн + вызов только при активном пейсинге
(раньше применялся даже с выключенным лимитером — нарушение pass-through контракта).

### 4. Watchdog ломал CLI-команды (логический пробел)
После UI-сессии `host_present=1` оставался в shared memory; CLI `limit`/`tune`/`bench` публикуют конфиг
и выходят → heartbeat протухал → DLL через 3 с игнорировала конфиг → **лимитер молча не включался**.
Фикс: `SharedRing::mark_headless()` + вызовы в cmd_limit, cmd_tune, bench, UI detach/Drop.

### 5. Флакующий тест поймал недоинициализацию (бонус)
`heartbeat_watchdog_lifecycle` падал при повторных прогонах: `SharedRing::init` не обнулял новые поля
(alloc не гарантирует нули). В проде CreateFileMappingW даёт нули, но API обязан быть детерминированным.
Добавлена явная инициализация в init.

### 6. Мелочи
- Честные входы тюнера в UI: `hold_max` считался как `p50*1.5` (выдумка), `late_count` у OFF = 0. Теперь реальные `hold_max_us`/`late` из SideStats (добавлены в live.rs).
- `--version`/`-V`, `watch-etw` в usage, `limit --hold` (резидентный holder heartbeat'а — нужен и пользователям CLI, и для E2E-проверки watchdog'а).
- refresh-override теперь сохраняется в профиль (`GameProfile.refresh_hz`, serde default — старые profiles.toml совместимы).

## Новые тесты
- `vtable_slot_indices_match_real_com_objects` (iframe-hook): создаёт настоящий D3D11-свапчейн и сверяет
  каждый хардкод-слот с адресами полей типизированной vtbl из windows-крейта. **Именно этот тест поймал бы
  баги 1–2 до релиза.** ✅ ok
- `heartbeat_watchdog_lifecycle` (iframe-common): полный жизненный цикл watchdog'а. ✅ ok (после фикса 5, стабильно 5/5)
- lmtrust_profiles: roundtrip + default для `refresh_hz`. ✅ ok

## Верификация
- `cargo build --release` — ✅ Finished
- `cargo test --workspace --release` — ✅ **114 passed / 0 failed** (было 100/1)
- Живой E2E (d3d11_test_app, PID 11668):
  - inject → `OK: hooks installed` — лог: `Present@8, ResizeBuffers@13, Present1@22, factory create@10/15/16/24`
  - `SetMaximumFrameLatency(1) applied via IDXGISwapChain2` — фикс 3 подтверждён в живом процессе
  - headless `limit --fps 40 --refresh 240` → 40.0–41.1 FPS, p50 25.0 мс, late=false (E2E-1 ✅)
  - `limit --hold` (holder жив) → ровно 40.0 FPS
  - **kill holder → через 4.5 с лимитер самоотключился: 6749/5673 FPS** (E2E-2 ✅ — главный сценарий краша UI закрыт)

## Осталось (не блокирует)
- iframe-solver: «SAT» в тюнере декоративен (решение игнорируется, диагноз — обычные if'ы); `/arch:AVX2` в build.rs уронит процесс на CPU без AVX2 (хост-сайд, не критично); ожидания эффекта (jitter/latency) — эвристики, подавать как оценки.
- `custom_refresh_hz` применяется и сохраняется в профиль, но в UI только пресеты (60/120/144/165/240) — произвольное значение ввести нельзя.
- `display_stats.clone()` каждый кадр UI (~500 КБ/с аллокаций) — терпимо, но можно убрать перекладыванием владения.
- Метки времени в UI-журнале — «с момента загрузки ОС» (QPC), а не настенные; мелочь.
- SM_VERSION не поднят при расширении хедера — совместимо (новые поля в хвосте, нули для старых сторон), но при следующем изменении layout — поднять.

## Изменённые файлы (эта сессия)
- crates/iframe-hook/src/hooks/dxgi.rs — слоты 22/15/16/24, типизированный SetMaximumFrameLatency, гейт на pacing, тест vtable
- crates/iframe-common/src/shared_mem.rs — mark_headless, явная инициализация watchdog-полей, тест heartbeat
- crates/iframe-app/src/live.rs — SideStats.late/hold_max_us, прокидывание в бакеты
- crates/iframe-app/src/ui.rs — честные входы тюнера, mark_headless в detach/Drop, refresh в профиль
- crates/iframe-app/src/profiles.rs — GameProfile.refresh_hz
- crates/iframe-app/src/main.rs — mark_headless в limit/tune, --hold, --version, usage
- crates/iframe-app/src/bench.rs — mark_headless
- crates/iframe-app/src/etw.rs — инициализатор SideStats
- crates/iframe-app/tests/lmtrust_profiles.rs — refresh_hz в тестах