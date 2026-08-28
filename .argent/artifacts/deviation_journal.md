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

## Состояние этапов
- [x] M0 — workspace, pacer.rs (13 unit-тестов ✅), shared_mem.rs (SPSC ✅), DLL-смоук ✅, git init ✅
- [x] M1 — инъекция ✅ (CreateRemoteThread+LoadLibraryW), vtable-хук Present@8/Present1@21 ✅, телеметрия ✅ (~64 FPS captured, процесс жив), hook_state через shared header ✅
- [ ] M2 — JIT-ядро в DLL + стенд D3D11 + автопроверки
- [ ] M3 — UI (egui, график, трей, профили)
- [ ] M4 — Flip Model: CreateSwapChain*/ResizeBuffers, waitable object (opt-in)
- [ ] M5 — D3D9 + x86
- [ ] M6 — ETW «только телеметрия» + чёрный список античитов
- [ ] M7 — стресс-тест, сравнение с RTSS, релиз