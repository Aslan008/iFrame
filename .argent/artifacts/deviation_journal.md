# Журнал отклонений (deviation journal)

Формат: дата · этап · что отклонилось от плана · почему · статус.

## 2026-08-29 · M0

1. **Rust 2024: `#[no_mangle]` → `#[unsafe(no_mangle)]`.** План второго ИИ использует `#[no_mangle]` и `static mut TRAMPOLINE` — в edition 2024 первое требует `#[unsafe(...)]`, второе запрещено (К3 подтверждено компилятором). Все экспорты DLL пишем через `#[unsafe(no_mangle)]`; trampoline — `AtomicPtr` (решено на M1).

2. **TelemetryFrame: `deviation_us` изменён с f64 на f32** — иначе запись 52 байта вместо расчётных 48; const-assert в коде теперь фиксирует 48 байт. Протокол телеметрии: v1, magic "IFRM".

3. **`SharedRing::init/attach` — явные `unsafe {}` блоки внутри unsafe-fn** (Rust 2024 `unsafe_op_in_unsafe_fn`). Ожидаемо; учтено для всех будущих unsafe-функций хука.

4. **Пейсер: outlier-clamp** `d ∈ [50 µs; 4·interval]` для EMA — защита от отравления оценки единичным статтером (в плане второго ИИ не было; добавлено при написании теста спайка).

5. **Пейсер: фильтр кадров < 50 µs** (вторичные свопчейны/дубли present не должны ломать EMA). Полноценная изоляция по swapchain — в M1.

6. **Подтверждено тестами:** каденс 40@120 = ровно каждый 3-й vblank; 50@120 = Брезенхэм 2/3 (среднее 2.4); GPU-bound → 100% bypass, ноль снов; wake ≥ present_end всегда (инвариант «кадр не удерживается»).

## Состояние этапов
- [x] M0 — workspace, pacer.rs (13 unit-тестов ✅), shared_mem.rs (SPSC ✅), DLL-смоук ✅, git init ✅
- [ ] M1 — hook DLL (pass-through Present) + инжектор + телеметрия
- [ ] M2 — JIT-ядро в DLL + стенд D3D11 + автопроверки
- [ ] M3 — UI (egui, график, трей, профили)
- [ ] M4 — Flip Model: CreateSwapChain*/ResizeBuffers, waitable object (opt-in)
- [ ] M5 — D3D9 + x86
- [ ] M6 — ETW «только телеметрия» + чёрный список античитов
- [ ] M7 — стресс-тест, сравнение с RTSS, релиз