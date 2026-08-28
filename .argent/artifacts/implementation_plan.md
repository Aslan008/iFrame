# iFrame — ОБЪЕДИНЁННЫЙ план реализации (v2, финальный)

Синтез моего плана и плана ИИ-помощника (`~/.gemini/antigravity-ide/brain/bcc2c11b.../implementation_plan.md`).
Ядро совпадает: **JIT Start Pacing** — реальный Present выполняется мгновенно, сон ПОСЛЕ него, следующий кадр стартует так, чтобы завершиться точно к vblank. Ноль удержания кадров.

## Что взято из плана второго ИИ (улучшения против моей v1)
1. **Формализованная математика пейсера**: EMA(α=0.15) + EMA-дисперсия(β=0.10) + адаптивный запас `S = S_base(0.3мс) + k(2.0)·σ`; bypass при `d ≥ T − S_base`. Беру как есть.
2. **Override SyncInterval**: при активном пейсере вызывать оригинальный Present с `SyncInterval=0`, чтобы исключить двойное ожидание (их VSync + наш таймер). Беру с исправлениями — см. Корректировка №1.
3. **Иерархия детекции vblank**: `IDXGIOutput::GetFrameStatistics` → `D3DKMTGetScanLine/WaitForVerticalBlankEvent` → `DwmGetCompositionTimingInfo` → QPC free-run. Беру (у меня был только DWM→D3DKMT).
4. **Отключение ProcessPowerThrottling** для потока хука. Беру.
5. **Milestone M4 (Flip Model / waitable object)** отдельным этапом. Беру.

## Корректировки к плану второго ИИ (критично)

### К1. `DXGI_PRESENT_ALLOW_TEARING` нельзя ставить безусловно
Их код: `flags | DXGI_PRESENT_ALLOW_TEARING` — на свопчейне, созданном без флага `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING`, это вернёт `DXGI_ERROR_INVALID_CALL` → игра без VSync сломается.
**Исправление:** перед override запросить `GetDesc1/GetDesc`.Flags; флаг ставится только если свопчейн создан с ALLOW_TEARING. Если нет — override только `SyncInterval=0` (в borderless/flip DWM всё равно композитит по vblank — разрыва не будет).
**Исправление 2:** для D3D9 exclusive fullscreen override `SyncInterval` НЕ делаем по умолчанию (там SyncInterval=0 = реальный разрыв кадра) — только режим «фазового выравнивания» с сохранением SyncInterval игры. Override для D3D9 — опция per-game.

### К2. В их сниппете пейсера нет каденса для FixedVsync
`vblank_qpc_hint` = ближайший vblank → при FPS < частоты монитора пейсер выдавал бы кадр КАЖДЫЙ vblank (FPS = частота монитора, лимит не работает).
**Исправление:** полный расчёт каденса: `N = round(refresh/target)`; при нецелых отношениях (50 FPS @ 120 Гц) — распределение Брезенхэма по N-м vblank (как консоли). Цель — всегда N-й vblank, не ближайший.

### К3. `static mut TRAMPOLINE` не скомпилируется в Rust 2024 (запрет static mut refs)
**Исправление:** `AtomicPtr`/`OnceLock` для trampoline и глобального состояния хука; `DllMain` без аллокаций в DLL_PROCESS_ATTACH (инициализация в отдельном потоке).

### К4. D3D9 выпал из этапов (а пользователь выбрал DXGI + D3D9)
**Исправление:** возвращаю отдельный этап с хуками `IDirect3DDevice9::Present/PresentEx`, стендом `d3d9_test_app` и `SetMaximumFrameLatency(1)` через IDirect3DDevice9Ex.

### К5. Добавления из моего плана
- **x86 + x64 сборки DLL** (многие D3D9-игры 32-битные; инжектор должен матчить разрядность).
- **Хук ResizeBuffers** в M4: при форсировании флага WAITABLE_OBJECT игра, вызывающая `ResizeBuffers` без флага, получит ошибку → краш. Хук чинит флаги (так делает SpecialK). Фича per-game opt-in, default OFF.
- **Критерий нулевой задержки** дополнен: время от входа в хук до реального Present < 0.05 мс И время хука после возврата Present не влияет на кадр (сон строго после сдачи в очередь).
- **Реалистичные пороги**: σ<0.1 мс — цель для тестового стенда; для реальных игр — p99−p50 < 0.5 мс (DWM вносит свой джиттер).

## Структура workspace (объединённая, без изменений по сути)
```
iFrame/
├── Cargo.toml                    # workspace, resolver = "2"
├── crates/
│   ├── iframe-common/            # pacer.rs (JIT, EMA, σ, cadence), shared_mem.rs (SPSC ring), config.rs
│   ├── iframe-hook/              # cdylib: hooks/{dxgi,d3d9}.rs, timing/{high_res,vblank}.rs, telemetry.rs
│   └── iframe-app/               # injector.rs, process_watcher.rs, etw_monitor.rs, ui/{app,graph,tray}.rs, settings.rs
└── tests/
    ├── d3d11_test_app/           # вращающийся 3D + искусственная CPU/GPU нагрузка
    ├── d3d9_test_app/
    └── pacer_benchmarks/         # автотесты ровности/задержки
```

## Этапы
- **M0.** Линкер-смоук (cdylib), workspace, `pacer.rs` + unit-тесты (стабильный 60 FPS, спайки 20 мс, GPU-bound, каденс 40@120, 50@120), `shared_mem.rs`.
- **M1.** hook DLL (pass-through Present DXGI, vtable через dummy device, AtomicPtr-trampoline) + инжектор (CreateRemoteThread+LoadLibraryW, x64) + телеметрия в консоль → инъекция в d3d11_test_app без падений.
- **M2.** JIT-ядро: HighResSleeper, vblank-иерархия, каденс, override SyncInterval (с К1), MaxFrameLatency(1) → автопроверки на стенде: hold<0.05мс, σ фреймтайма, bypass при перегрузке.
- **M3.** UI: eframe/egui (тёмная тема), график 10с без аллокаций, статистика (FPS, jitter, margin), трей (tray-icon), хоткеи, профили `%APPDATA%\iFrame\profiles.toml`.
- **M4.** Flip Model: хук CreateSwapChain*/ResizeBuffers, waitable object (opt-in per-game), SetMaximumFrameLatency(1), синхронизация по waitable handle.
- **M5.** D3D9: хуки, стенд, тесты, x86-сборка DLL + x86-инъекция.
- **M6.** ETW «только телеметрия» (Microsoft-Windows-DxgKrnl, дефолтный режим) + чёрный список античитов (EAC/BattlEye/Vanguard/FACEIT/Ricochet) с блокировкой инъекции.
- **M7.** Финализация: стресс-тест 2ч (0 крашей/утечек), сравнение с RTSS FES и внутриигровым лимитером на стенде, релизные бинарники.

## Критерии успеха
| Критерий | Методика | Цель |
|---|---|---|
| Добавленный лаг ввода | t_input→t_present vs чистое d | **0.0 мс добавки** |
| Удержание кадра в хуке | время до реального Present | **< 0.05 мс** |
| Ровность (стенд) | present-to-present | **σ < 0.1 мс, p99−p50 < 0.2 мс** |
| Ровность (реальная игра) | p99−p50 | **< 0.5 мс** |
| GPU-bound | перегрузка шейдерами | **0 пропущенных vblank, мгновенный bypass** |
| Античит | запуск с EAC-игрой | **инъекция заблокирована, ETW работает** |
| Стабильность | 2ч стресс D3D11/D3D9 | **0 крашей, 0 утечек** |

## Протокол совместной работы (я + второй ИИ)
- Канонический план и журнал отклонений — в `.argent/artifacts/` (implementation_plan.md — этот файл; deviation_journal.md — ведётся с M0).
- Каждый milestone заканчивается проверяемым артефактом (тесты/стенд) — второй ИИ ревьюит до перехода к следующему.
- Все расхождения с планом второго ИИ фиксируются в deviation_journal.md с обоснованием (К1–К5 уже внесены).