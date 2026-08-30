# iFrame — полный анализ проекта (2026-08-30)

Режим: только анализ, изменения в код не вносились (кроме диагностического `.argent/check_output.txt` от `cargo check`).

---

## 1. Что это

**iFrame v1.0.0** — Windows FPS-лимитер с нулевой добавленной задержкой ввода (JIT Start Pacing).
Ключевое отличие от RTSS/драйверных лимитеров: реальный `Present` выполняется мгновенно, сон происходит **после** него и задерживает только **старт следующего кадра**, выравнивая его по сетке vblank (Брезенхэм-каденс при нецелых отношениях FPS/Гц).

Статус: **все 7 milestone'ов плана завершены** (M0–M7, релиз v1.0.0, коммит `9fd594c`). Журнал отклонений ведётся с M0 (`.argent/artifacts/deviation_journal.md`, 7 этапов).

## 2. Структура и архитектура

Cargo workspace, edition 2024, `resolver = "2"`, release: `lto = thin`, `codegen-units = 1`.

```
crates/
  iframe-common/   # чистая математика + IPC-протокол (без WinAPI, тестируемая)
    pacer.rs       # JitPacer: EMA(α=0.15) + deviation(β=0.10) + адаптивный запас,
                   #   режимы Vrr / FixedVsync (Брезенхэм) / Bypass,
                   #   инвариант wake ≥ present_end (кадр никогда не удерживается)
    shared_mem.rs  # lock-free SPSC-кольцо в file mapping Local\iFrameSM_<pid>,
                   #   конфиг app→DLL через атомики заголовка (48-байт TelemetryFrame)
    config.rs      # RuntimeConfig (шлётся в DLL) + GameProfile (не используется!)
  iframe-hook/     # cdylib, инъектируется в игру
    hooks/dxgi.rs  # vtable-патч Present@8 / Present1@21 / ResizeBuffers@13 +
                   #   фабрика CreateSwapChain@10/14/15/23 (капы свопчейна из desc,
                   #   К1: tearing-aware override SyncInterval)
    hooks/d3d9.rs  # template-патчинг vtable в образе d3d9.dll + daemon ре-патча (1 ч)
    engine.rs      # пейсинг-движок: try_lock (мультипоточный Present → skip), кап сна 100 мс
    timing.rs      # QPC + гибридный sleeper (CREATE_WAITABLE_TIMER_HIGH_RESOLUTION + спин 400 мкс)
    vblank.rs      # DwmGetCompositionTimingInfo → VBlankHint
    telemetry.rs   # attach/create shared memory, запись телеметрии
  iframe-app/      # bin iframe.exe: GUI (eframe 0.36/egui) + CLI
    injector.rs    # CreateRemoteThread + LoadLibraryW, IsWow64Process → выбор x86/x64 DLL
    anticheat.rs   # 3 уровня чёрного списка, проверка ДО любого доступа к процессу
    etw.rs         # ferrisetw, DxgKrnl Present-события (42/126/168/172/175/184) — режим без инъекции
    ui.rs / tray.rs / live.rs / profiles.rs / sm_host.rs / watch.rs
tests/
  d3d11-test-app/  # стенд: flip-модель, опции --cpu-ms/--vsync/--tearing/--seconds
  d3d9-test-app/   # стенд D3D9 (+ --delay-device, диагностика IAT-слота)
  pe_dump.py       # PE-дамп импорт-таблиц (диагностика D3D9-хуков)
```

Поток данных: `iframe.exe` создаёт mapping → инжект DLL → DLL аттачится, ставит vtable-хуки, публикует `hook_state` в shared header → на каждый Present: QPC → реальный Present → (если pacing) DWM-hint → `engine::pace()` → сон до `wake_qpc` → телеметрия в кольцо → app дренажит кольцо (поток live.rs, 20 мс) → график/статистика.

## 3. Верификация (выполнено сейчас, фактически)

| Проверка | Результат |
|---|---|
| `cargo check --workspace --all-targets` | **Успешно** (`Finished dev profile in 2.38s`), ~110 предупреждений, 0 ошибок |
| `cargo test -p iframe-common` | **13 passed; 0 failed** (пейсер + SPSC-кольцо) |
| `git status` | M: `main.rs`, `ui.rs` (незакоммичено), `?? exports/`, M `.argent/compact_log.jsonl` |
| Бинарники | `target/release/` (iframe.exe, iframe_hook.dll x64; i686 DLL собирается отдельно) |

Предупреждения: `iframe-hook` — 89 (подавляющее большинство — `unsafe_op_in_unsafe_fn` E0133: в edition 2024 unsafe-операции внутри `unsafe fn` требуют явных `unsafe {}`-блоков; это warnings, не ошибки), `iframe-app` — 11, тест-стенды — 6, `iframe-common` — 2 (неиспользуемые импорты в тестах).

## 4. Сильные стороны

1. **Чистая архитектура**: математика пейсера полностью отделена от WinAPI и покрыта симуляционными тестами (стабильный 60@120, каденс 40@120 = ровно 3, 50@120 = Брезенхэм 2/3, спайк 20 мс, GPU-bound → 100% bypass, инвариант wake ≥ present_end).
2. **Безопасность горячего пути**: lock-free (`try_lock` → skip при конкуренции), без аллокаций, кап сна 100 мс + ограниченный таймаут таймера + спин-доводка — игра физически не может зависнуть в хуке.
3. **Опыт крашей задокументирован и залечен**: неправильный каст COM-обёртки (c0000005), вечное ожидание незаряженного таймера, ловушка GetDesc1 (слот 17 за пределами базовой vtable), стартовый краш трея до event loop.
4. **Anti-cheat гейт** стоит первым шагом в `injector::inject` и `ui::attach` — до `OpenProcess`.
5. **Кросс-процессное состояние только через shared header** (урок M1: hook_state).
6. **ETW telemetry-only** как безопасный фолбэк для защищённых игр.

## 5. Найденные проблемы (по убыванию важности)

### Функциональные
1. **Чекбокс «override VSync» в UI — no-op.** `RuntimeConfig` не содержит `vsync_override`; хук применяет К1-override всегда при активном пейсинге. Настройка сохраняется в профиль, но никогда не доходит до DLL. Либо убрать чекбокс, либо протянуть поле через shared header.
2. **`RuntimeConfig.refresh_hz` — мёртвый конфиг.** Публикуется в shared memory, CLI принимает `--refresh`, но `engine::pacer_config()` его игнорирует — refresh всегда берётся из DWM-hint. `--refresh` ничего не делает.
3. **`iframe-common::config::GameProfile` (с `force_waitable_object`) не используется нигде** — приложение имеет собственный `profiles::GameProfile`. Заложенная в M4 фича force-waitable-object (per-game opt-in) не реализована в хуке.
4. **`SharedConfig` в shared_mem.rs — мёртвый код** (конфиг-канал работает через `RuntimeConfig`).
5. **`anticheat.rs`: `"netsh.exe"` в списке PROTECTED_PROCESSES** — почти наверняка ошибка (netsh — сетевая утилита Windows, не античит). Ложное срабатывание заблокирует инъекцию в безобидный процесс.
6. **`cmd_inject`: `let _ = mapping;` уничтожает mapping немедленно**, вопреки комментарию «keep the mapping alive until injection completes» (`let _ = x` дропает значение сразу). Работает случайно: DLL при неудаче `OpenFileMappingW` создаёт свой mapping с тем же именем, app затем пере-открывает по имени. Есть гонка: между дропом и аттачем DLL секция уничтожена → DLL идёт по standalone-ветке. В `ui::attach()` mapping держится в `self.mapping` — там корректно.
7. **ETW-сессия при закрытии по Ctrl+C**: в `cmd_watch_etw` цикл бесконечен, `watch.stop()` — unreachable code; обработчика Ctrl+C нет. Реалтайм-сессии обычно чистит ОС при смерти процесса, но код-путь остановки мёртв.

### Качество/техдолг
8. **~110 warnings**, из них 89 в `iframe-hook` — почти все `unsafe_op_in_unsafe_fn` (edition 2024). Явные `unsafe {}`-блоки внутри unsafe-fn сделаны только в `shared_mem.rs` (M0), хуки писались без них. В будущих редакциях Rust это станет жёсткой ошибкой.
9. **Мёртвый код**: `default_dll_path()` в main.rs и ui.rs, `type ThreadStart` в injector.rs, `WINDOW_TITLE` в обоих стендах, `TelemetryFrame::set_bypass`, неиспользуемые импорты (`HANDLE`, `FARPROC`, `TraceTrait`, `IDXGIFactory2`, `VirtualProtect`+... в d3d9.rs), `stop_cb`/`mut watch`/`mut t` в etw.rs.
10. **UI-фриз при attach**: `ui::attach()` ждёт hook_state в блокирующем цикле (до 5 с, sleep 30 мс) на UI-потоке.
11. **`profiles.save()` на каждое изменение UI**: DragValue FPS при перетаскивании перезаписывает TOML-файл десятки раз в секунду.
12. **`live.rs`-воркер**: клонирует весь deque сэмплов и аллоцирует `Vec` статистики каждые 20 мс (для UI терпимо; заявленная в плане «график без аллокаций» не выполнена буквально).
13. **D3D9**: хук покрывает только девайсы, созданные в окне после инсталляции (template-патч + daemon ре-патча 1 ч); d3d9.dll строит vtable динамически и откатывает патчи — устойчивое покрытие требует inline-хуков экспортов (задокументировано в README/journal как бэклог). `uninstall()` восстанавливает только templates[0].
14. **Имя теста `overflow_drops_oldest_policy`** описывает обратное: при переполнении дропается **новая** запись (кольцо хранит старые). Поведение осознанное, имя вводит в заблуждение.
15. **Незакоммиченные правки** в `main.rs`/`ui.rs` (упрощение EnumWindows-sink: Arc<Mutex> → сырой указатель + `let _ = EnumWindows`) — рабочие, но не зафиксированы в git; `exports/` не трекается.

### Ограничения среды (не баги)
- ETW требует элевации (0x80070005 в `etw_err.txt` — подтверждено реальным запуском без админа); живой тест ETW не проводился (UAC-промпты отклонялись).
- DWM троттлит фоновые окна до ~64 Гц — точный каденс верифицируется только на foreground-окне.
- Тесты пейсера — симуляция; критерий «удержание < 0.05 мс» из плана не автоматизирован (замерялся вручную в M2).

## 6. Зависимости

windows 0.61 (единый workspace-деп с фичами), eframe 0.36 (glow, без default features), egui_plot 0.37, tray-icon 0.24, global-hotkey 0.8, serde 1 + toml 0.9, ferrisetw 1.2.0. Cargo.lock зафиксирован.

## 7. Резюме

Проект в зрелом, релизном состоянии (v1.0.0): ядро (пейсер + shared memory) — чистое, протестированное, с сильной дисциплиной документирования решений. Основные хвосты: несвязанный UI-переключатель vsync_override, мёртвые конфиги refresh_hz/GameProfile/SharedConfig, опечатка `netsh.exe` в античит-списке, гонка с дропом mapping в CLI-инъекте и массовые `unsafe_op_in_unsafe_fn`-warnings. Ничего из этого не блокирует использование DXGI-пути (основного) на x64/x86.