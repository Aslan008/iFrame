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

## Состояние этапов
- [x] M0 — workspace, pacer.rs (13 unit-тестов ✅), shared_mem.rs (SPSC ✅), DLL-смоук ✅, git init ✅
- [x] M1 — инъекция ✅, vtable-хук Present@8/Present1@21 ✅, телеметрия ✅, hook_state через shared header ✅
- [x] M2 — JIT-ядро в DLL ✅: лимит 40 FPS живьём (41.1/39.9/40.2/40.1), каденс Брезенхэма по сетке vblank (DWM 240 Гц), override SyncInterval (К1, только windowed), отключение → возврат 64.3 FPS, процесс жив, 13/13 тестов ✅
- [x] M3 — UI ✅: egui-окно (график 10с + линия цели, статистика FPS/p50/p99/late, слайдер+пресеты FPS, режимы ZeroLag/VRR/Off, vsync override), трей (показать/скрыть, toggle, quit), глобальный хоткей Ctrl+Alt+I, профили per-exe в %APPDATA%\iFrame\profiles.toml, авто-аттач известных игр, always-on-top; UI жив 5+ мин, рендер подтверждён скриншотом
- [ ] M4 — Flip Model: CreateSwapChain*/ResizeBuffers, waitable object (opt-in)
- [ ] M5 — D3D9 + x86
- [ ] M6 — ETW «только телеметрия» + чёрный список античитов
- [ ] M7 — стресс-тест, сравнение с RTSS, релиз