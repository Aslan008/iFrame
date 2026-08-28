# iFrame — трекер задач

**Статус: M0 ЗАВЕРШЁН** (коммит 67ff2a6)

## Что сделано в M0
- Cargo workspace: `iframe-common` / `iframe-hook` (cdylib) / `iframe-app` (bin), edition 2024, release: LTO thin.
- `pacer.rs` — ядро JIT-пейсинга: EMA + deviation + адаптивный запас, режимы Vrr / FixedVsync (Брезенхэм-каденс) / Bypass, инвариант «wake ≥ present_end» (кадр никогда не удерживается).
- `shared_mem.rs` — lock-free SPSC-кольцо в shared memory + конфиг-канал app→DLL через атомики заголовка.
- `config.rs` — RuntimeConfig / GameProfile.
- `iframe_hook.dll` собирается и экспортирует `iframe_ping`/`iframe_abi_version` (проверка линкера ✅).
- git-репозиторий инициализирован.

## Проверка (реальный вывод)
```
cargo test -p iframe-common → 13 passed; 0 failed
iframe-app.exe → "iFrame v0.1.0 — zero-added-latency frame pacer"
артефакты: iframe_hook.dll 103.5 KB, iframe-app.exe 144.5 KB
```

## Следующий шаг: M1 (сквозной путь инъекции)
1. `iframe-hook`: dummy D3D11 device → vtable IDXGISwapChain → vtable-patch Present/Present1 (AtomicPtr trampoline, `#[unsafe(no_mangle)]`-экспорты, init в отдельном потоке из DllMain).
2. `iframe-app`: инжектор CreateRemoteThread+LoadLibraryW, открытие `Local\iFrameSM_<pid>` до инъекции.
3. Телеметрия: DLL пишет TelemetryFrame на каждый Present (pass-through, без пейсинга).
4. Проверка: инъекция в d3d11_test_app (написать минимальный стенд), Present перехватывается, процесс жив, телеметрия течёт в консоль.

**Ожидает ревью второго ИИ перед стартом M1.**