# Журнал отклонений (deviation journal)

Формат: дата · этап · что отклонилось от плана · почему · статус.

## 2026-08-29 · M0

1. **Rust 2024: `#[no_mangle]` → `#[unsafe(no_mangle)]`.** План второго ИИ использует `#[no_mangle]` и `static mut TRAMPOLINE` — в edition 2024 первое требует `#[unsafe(...)]`, второе запрещено (К3 подтверждено компилятором). Все экспорты DLL пишем через `#[unsafe(no_mangle)]`; trampoline — `AtomicPtr` (решено на M1).

2. **TelemetryFrame: `deviation_us` изменён с f64 на f32** — иначе запись 52 байта вместо расчётных 48; const-assert в коде теперь фиксирует 48 байт. Протокол телеметрии: v1, magic "IFRM".

3. **`SharedRing::init/attach` — явные `unsafe {}` блоки внутри unsafe-fn** (Rust 2024 `unsafe_op_in_unsafe_fn`). Ожидаемо; учтено для всех будущих unsafe-функций хука.

4. **Пейсер: outlier-clamp** `d ∈ [50 µs; 4·interval]` для EMA — защита от отравления оценки единичным статтером (в плане второго ИИ не было; добавлено при написании теста спайка).

5. **Пейсер: фильтр кадров < 50 µs** (вторичные свопчейны/дубли present не должны ломать EMA). Полноценная изоляция по swapchain — в M1.

6. **Подтверждено тестами:** каденс 40@120 = ровно каждый 3-й vblank; 50@120 = Брезенхэм 2/3 (среднее 2.4); GPU-bound → 100% bypass, ноль снов; wake ≥ present_end всегда (инвариант «кадр не удерживается»).

## 2026-08-29 · M1

1. **windows crate 0.61: имена фич и модулей.** Фича называется `Win32_Graphics_Direct3D11` (не `D3D11`); модуль `Graphics::Direct3D11`; `D3D_DRIVER_TYPE`/`D3D_FEATURE_LEVEL` — в `Graphics::Direct3D`; `DXGI_MODE_DESC/SAMPLE_DESC/RATIONAL/FORMAT` — в `Graphics::Dxgi::Common`; `BOOL` — в `windows::core` (не Foundation); `HMODULE/HINSTANCE` — в Foundation (не core). Функции windows-крейта компилируются только если включены фичи ВСЕХ типов в их сигнатурах: `CreateRemoteThread`/`CreateWaitableTimerExW`/`CreateFileMappingW` потребовали `Win32_Security` (SECURITY_ATTRIBUTES), `D3D11CreateDeviceAndSwapChain` — `Win32_Graphics_Dxgi_Common`. Урок: гейт функции = объединение фич всех типов её сигнатуры.

2. **MapViewOfFile возвращает `MEMORY_MAPPED_VIEW_ADDRESS`** (структура с `.Value`), а не `Result<*mut c_void>`; `UnmapViewOfFile` принимает ту же структуру. `VirtualAllocEx` возвращает сырой указатель (не Result).

3. **DefWindowProcW в 0.61 — Rust unsafe fn**, не extern "system" → для WNDCLASSW.lpfnWndProc нужна обёртка `unsafe extern "system" fn`. `GetModuleHandleW(None)` не компилируется (P0: Param<PCWSTR>) → `PCWSTR::null()`; CreateWindowExW ждёт `Option<HINSTANCE>` → конверсия `HINSTANCE(hmodule.0)`.

4. **БАГ найден сквозным тестом:** DLL публиковала hook_state в процесс-локальную атомику, а приложение читало из shared memory → вечный state=0 при фактически установленных хуках (лог DLL это подтвердил). Исправлено: `ring.set_hook_state(1/2)` в hooks::install(). Урок: кросс-процессное состояние — только через shared header.

5. **Порядок аргументов D3D11CreateDeviceAndSwapChain** (0.61): `(..., ppswapchain, ppdevice, pfeaturelevel, ppimmediatecontext)` — out-параметр pfeaturelevel идёт ПЕРЕД контекстом; pfeaturelevels — `Option<&[...]>`.

6. **Эмпирика стенда:** uncapped Present(0) на flip-модели в окне троттлится DWM до ~64 FPS (p50 15.6 мс) — это поведение DWM, не бага; для M2-тестов ровности это удобная база.

## 2026-08-29 · M2

1. **КРАШ НАЙДЕН И ПОБЕЖДЁН (главный урок M2):** `&*(this as *const IDXGISwapChain)` — НЕВЕРНЫЙ способ получить обёртку из сырого COM-указателя. Обёртка windows-крейта *содержит* указатель на объект как поле; такой каст заставляет её внутреннее поле прочитать первые 8 байт объекта (= указатель на vtable) как будто это указатель на объект → все COM-вызовы через неё разыменовывают мусор → access violation (подтверждено Event Log: c0000005 в iframe_hook.dll, offset 0xb508, ровно на первом paced-present). Исправлено: ручной вызов GetDesc через vtable (слот 12) с явным чтением vtbl из объекта. Правило: сырой COM this → только ручные vtable-вызовы или `from_raw_borrowed`.

2. **Бесконечное ожидание в sleeper:** `SetWaitableTimer` мог не сработать → `WaitForSingleObject(INFINITE)` на незаряженном таймере = вечное зависание игрового потока (первый краш-паттерн: 3 presents → тишина). Исправлено: проверка результата SetWaitableTimer + ограниченный таймаут (remaining + 2мс) + спин-доводка; плюс жёсткий кап сна 100мс (`sleep_until_capped`) — пейсер физически не может усыпить игру дольше.

3. **DWM rateRefresh = 240 Гц** (реальная герцовка монитора), но фоновое окно стенда DWM композитит на ~64 Гц (троттлинг неактивных окон) → фактический каденс квантуется границами композита. Для реальной игры на переднем плане compose = refresh. Это тестовая особенность, не баг ядра.

4. **Адаптивный запас работает в бою:** margin вырос 300µs → 5.6ms под джиттером d (5–17.6ms из-за compose-троттлинга), все 12 замеров `late=false`, bypass при перегрузке подтверждён.

5. **debug_step (квота 40) оставлен в хуке** — атомарный счётчик ~1ns, замолкает сам; пригодится для диагностики на реальных играх.

6. **Диагностический протокол:** Event Log (Application, Id=1000) даёт код исключения + сбойный модуль + RVA — это быстрее гаданий; пошаговый debug_step в хуке локализует краш за один прогон.

## 2026-08-29 · M3

1. **eframe 0.36 — полностью новый App-трейт:** главного метода больше нет `update(ctx, frame)` — теперь `fn ui(&mut self, ui: &mut egui::Ui, frame)` (приложение получает Ui напрямую) + отдельный `fn logic(&mut self, ctx, frame)` для контекстных операций (viewport-команды, репейнт, поллинг трея). Панели: `TopBottomPanel` удалён → единый `egui::Panel::top/bottom(...).show(&mut Ui, ...)` — панели теперь принимают `&mut Ui`, а не `&Context`.

2. **СТАРТОВЫЙ КРАШ UI (одноразовый, найден скриншот-бисекцией):** создание tray-icon + global-hotkey в конструкторе приложения (до старта winit event loop) → AV c0000005 в iframe.exe при первом запуске; второй запуск с тем же бинарём — стабилен. Исправлено: ленивая инициализация трея/хоткеев на первом кадре `ui()`, когда event loop уже качает сообщения и скрытое окно трея обслуживается. Урок: Win32-объекты с собственными окнами сообщений (tray, hotkey) создавать только при уже качающемся цикле.

3. **tray-icon 0.24** реэкспортирует muda как `tray_icon::menu`; `Menu::new()`/`MenuItem::new()` без Result (append_items — с Result). **global-hotkey 0.8**: id хоткея — просто `u32` (HotKey::id()), не отдельный тип.

4. **egui_plot 0.37** совместим с egui 0.36 (eframe 0.36) ✓; `LineStyle::Dashed { length }` — вариант энума, не конструктор; `Line::new(name, points)`.

5. **Диагностика молчаливых крашей:** background-хост не захватывает stderr GUI-процессов — редирект `2> файл` внутри команды обязателен; Event Log (Id=1000) даёт сбойный модуль + RVA даже без вывода.

6. **Скриншот-верификация UI:** PowerShell System.Drawing CopyFromScreen → PNG → view_image — визуальное подтверждение рендера (график, оси, пунктирная линия цели, статус-бар, on-top тумблер).

## 2026-08-29 · M4

1. **ЛОВУШКА GetDesc1 (слот 17):** ручной вызов GetDesc1 через vtable, прочитанную из объекта по offset 0, вернул S_OK с ПОЛНОСТЬЮ нулевым desc (flags=0x0, buffers=0). Причина: vtable-указатель, хранящийся в объекте, — базовая IDXGISwapChain vtable (17 слотов, 0–16); слот 17 читает ПАМЯТЬ ЗА vtable → мусорный вызов (по сигнатуре — чужой QI) → S_OK + нули. Урок: слоты ЗА пределами базового интерфейса нельзя вызывать через vtable, прочитанную из объекта, — нужен QI для нужного интерфейса. Обход: в legacy DXGI_SWAP_CHAIN_DESC (GetDesc, слот 12 — работает) есть поле Flags → ОДИН вызов даёт windowed + ALLOW_TEARING (2048) + waitable (64). GetDesc1 не нужен вовсе.

2. **Хуки фабрики** (IDXGIFactory2 vtable: CreateSwapChain@10, ForHwnd@14, ForCoreWindow@15, ForComposition@23): капы нового свопчейна берутся ПРЯМО из входного desc хука — ноль COM-вызовов; новейший свопчейн становится активным (лаунчеры создают свои раньше игры). Фабрика добывается из дамми-девайса: device.cast::<IDXGIDevice>() → GetAdapter() → GetParent::<IDXGIFactory2>().

3. **TEARING РАБОТАЕТ — композит полностью обходится:** uncapped Present(0)+ALLOW_TEARING = **6096 FPS** (p50 108µs) — очередь/DWM не участвуют. Это доказывает: с флагом tearing кадр уходит straight-to-scanout.

4. **DWM-троттлинг фоновых окон — ограничение стенда:** окно, запущенное из агентской сессии, не может удержать foreground (Windows foreground lock) → DWM композитит его на ~64.5 Гц → presents квантуются сеткой композита (15.4/30.8мс) ДАЖЕ с tearing-флагом в Present. Пейсер при этом работает точно: late=false везде, Брезенхэм по сетке, адаптивный margin 300µs → 6.4мс. Точный каденс 25мс верифицируется на foreground-окне (реальная игра / ручной запуск стенда пользователем).

5. **Константы DXGI:** ALLOW_TEARING = swapchain-флаг 2048 / present-флаг 512; FRAME_LATENCY_WAITABLE_OBJECT = 64. DXGI_SWAP_CHAIN_DESC1.Flags — u32 (в отличие от i32-новтайпа флага).

6. **ResizeBuffers@13** инвалидирует кэш капс → ленивый re-query на следующем paced-present (пересоздание свопчейна игрой после альт-таба/ресайза подхватывается автоматически).

## 2026-08-29 · M5

1. **x86 готов полностью:** i686-таргет установлен, DLL собирается (182 КБ), инжектор определяет разрядность цели через IsWow64Process и выбирает i686/x64 DLL автоматически (injector::default_dll_path_for). Стенд собирается для обеих архитектур.

2. **ГЛАВНЫЙ УРОК D3D9: per-device КОПИИ vtable в куче** (анти-хук мера MS, доказано эмпирически: два девайса одного процесса — разные кучные адреса 0x232c914f110 / 0x232c900a8f0, у DXGI vtable общая в .rdata). Патч одного девайса не влияет на другие.

3. **IAT-хук Direct3DCreate9 — патч держится, но вызов идёт мимо:** slot read-back подтвердил наш хук в IAT (0x7fff46a0cc30), но значение слота ПЕРЕД вызовом игры — снова оригинал (0x7fff3faae450). Патч тихо откатывается за секунды (анти-тампер). Попутно найдены и исправлены: IMAGE_IMPORT_BY_NAME.Hint (+2 байта перед именем), INT-thunk'и в загруженном образе могут быть RVA (не VA) — эвристика различения по величине относительно image base, raw-dylib синтетические дескрипторы могут лежать за declared import_size (боунд заменён на абсолютный кап).

4. **Template-патчинг: работает в окне инсталла, но d3d9 строит vtable девайсов ДИНАМИЧЕСКИ.** Сигнатурный скан образа d3d9.dll (16 указателей копии) находит шаблон(ы) — в одном прогоне 2 шт.; самотест (второй дамми-девайс сразу после патча) получает хук ✓; но девайс игры, созданный на 15-й секунде, имеет ОРИГИНАЛЬНЫЙ Present (0x7fff3fa84030 — одинаков во всех прогонах). Вывод: копии строятся не memcpy из статического шаблона — статический патчинг побеждён дизайном d3d9.

5. **Итог D3D9 в M5:** хук работает для девайсов, созданных в окне после инсталляции (самотест ✓); устойчивое покрытие требует inline-хука экспортов d3d9.dll (trampoline) — отдельная задача в бэклоге. Практический сценарий: аттач до создания девайса (auto-attach при старте процесса) частично покрывает.

6. **Диагностический протокол пополнен:** PE-дамп импорт-таблиц Python-скриптом (tests/pe_dump.py) — сравнение файловой таблицы с тем, что видит walk; read-back патченного слота; печать Present-слота игрового девайса из стенда.

## 2026-08-29 · M6

1. **Чёрный список античитов (anticheat.rs):** три уровня — имена сервисов EAC/BattlEye/Vanguard/FACEIT (PROTECTED_PROCESSES), имена модулей, загруженных в целевой процесс (PROTECTED_MODULES — ловит античит в ЛЮБОЙ игре), известные защищённые игры (PROTECTED_GAMES). Проверка check_process(pid) стоит ПЕРВЫМ шагом в injector::inject и в ui::attach — ДО любого доступа к процессу (даже OpenProcess на защищённую игру может быть флагом).

2. **ETW telemetry-only (etw.rs):** real-time сессия на Microsoft-Windows-DxgKrnl (GUID 802ec45a-...) через ferrisetw 1.2.0; Present-события (ID 42/126/168/172/175/184), фильтр по ProcessId-свойству события (ядро логирует под своим PID), frametime из дельт timestamp'ов → общий live-график. Ноль инъекции — безопасно для защищённых игр.

3. **ETW требует прав администратора (0x80070005 «Отказано в доступе»)** — то же требование, что у PresentMon. Код корректен; сообщение об ошибке улучшено с подсказкой «запустите iFrame от администратора». Живой тест не проведён (нужна элевация, UAC-промпты пользователь отклоняет) — ручная проверка: запустить iframe.exe от админа → `watch-etw --pid N`.

4. **UI-fallback:** attach() проверяет античит ДО sm_host::create_for_pid; защищённая цель → автоматический переход в telemetry-only ETW режим (attached=true, exe_name из имени процесса). detach() останавливает ETW-сессию.

5. ** ferrisetw API-заметки:** record.raw_timestamp() -> i64 (не timestamp()); TraceError не реализует Display (использовать {:?}); EventRecord в native/etw_types/event_record.rs; ETW-таймстампы в 100ns единицах (frametime = delta/10 µs).

## Состояние этапов
- [x] M0 — workspace, pacer.rs (13 unit-тестов ✅), shared_mem.rs (SPSC ✅), DLL-смоук ✅, git init ✅
- [x] M1 — инъекция ✅, vtable-хук Present@8/Present1@21 ✅, телеметрия ✅, hook_state через shared header ✅
- [x] M2 — JIT-ядро в DLL ✅: лимит 40 FPS живьём (41.1/39.9/40.2/40.1), каденс Брезенхэма по сетке vblank (DWM 240 Гц), override SyncInterval (К1, только windowed), отключение → возврат 64.3 FPS, процесс жив, 13/13 тестов ✅
- [x] M3 — UI ✅: egui-окно (график 10с + линия цели, статистика FPS/p50/p99/late, слайдер+пресеты FPS, режимы ZeroLag/VRR/Off, vsync override), трей (показать/скрыть, toggle, quit), глобальный хоткей Ctrl+Alt+I, профили per-exe в %APPDATA%\iFrame\profiles.toml, авто-аттач известных игр, always-on-top; UI жив 5+ мин, рендер подтверждён скриншотом
- [x] M4 — Flip Model ✅: хуки фабрики CreateSwapChain/ForHwnd/ForCoreWindow/ForComposition (активный свопчейн + капы из desc), ResizeBuffers + инвалидация капс, tearing-aware К1 (SyncInterval=0+ALLOW_TEARING), waitable-детекция; верифицировано: детекция капс (flags=0x800 → tearing=true), обход композита (6096 FPS uncapped), лимит 40 FPS держится, процесс жив
- [x] M5 — x86 ✅ (i686 DLL + инжектор с авто-выбором по разрядности + стенд x86); D3D9 ⚠️ частично: хук-цепочка реализована (IAT → CreateDevice → Present@17), самотест ✓, но d3d9.dll строит vtable девайсов динамически + откатывает патчи (анти-тампер) — устойчивое покрытие требует inline-хуков (бэклог)
- [x] M6 — чёрный список античитов ✅ (3 уровня: сервисы/модули/игры, проверка ДО любого доступа к процессу, в inject + UI); ETW telemetry-only ✅ (ferrisetw, DxgKrnl Present-события → live-график, ноль инъекции; требует админа — как PresentMon); UI-fallback: защищённая цель → авто-переход в ETW-режим
- [ ] M7 — стресс-тест, сравнение с RTSS, релиз