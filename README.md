# iFrame — zero-added-latency frame pacer

A Windows FPS limiter that **adds no input latency** — unlike classical
limiters (RTSS, in-driver caps), which hold *already finished* frames until
the target time.

## The core idea: JIT pacing, not frame holding

| | RTSS / driver caps | iFrame |
|---|---|---|
| Where it waits | **After** Present, holding the finished frame | **Before** the next frame starts |
| What the GPU does | Idles while the frame is held | Already has the frame |
| Input latency | + up to one frame | **+0** (the frame is submitted the moment it's ready) |

iFrame hooks the presentation call, lets the real `Present` execute
immediately, and only then sleeps until the next frame's scheduled start.
The game thread wakes up with fresh input exactly when the next frame
should begin. The result: console-smooth frametimes with zero added
latency.

## Features

- **Zero-latency JIT pacing** — wait after Present, delay only the next
  frame's start (Bresenham cadence on the display's vblank grid).
- **DXGI Flip Model support** — factory `CreateSwapChain*` hooks, active
  swapchain tracking, tearing-aware vsync override, `ResizeBuffers` handling.
- **x64 + x86** — the injector detects the target's bitness and picks the
  matching DLL automatically.
- **Live UI** — frametime graph (10 s), FPS/p50/p99 stats, limiter controls,
  per-game profiles (`%APPDATA%\iFrame\profiles.toml`), tray icon,
  global hotkey `Ctrl+Alt+I`, auto-attach.
- **Anti-cheat safety** — a three-level blacklist (anti-cheat services,
  loaded modules, known protected games). Protected targets are refused
  *before any process access* — injection into an anti-cheat game risks a
  permanent ban.
- **Telemetry-only ETW mode** — for protected games: observe frame presents
  via a `Microsoft-Windows-DxgKrnl` real-time trace (the same source
  PresentMon uses). Zero injection, nothing for an anti-cheat to flag.
  Requires an elevated process (same as PresentMon).

## Usage

GUI (default):

```
iframe.exe
```

CLI:

```
iframe.exe inject --window "Game Title"     # inject the hook
iframe.exe inject --pid 1234                # by PID
iframe.exe limit --pid 1234 --fps 40        # set the limiter
iframe.exe limit --pid 1234 --fps 0         # disable
iframe.exe watch --pid 1234 --seconds 5     # live frametime stats
iframe.exe watch-etw --pid 1234             # telemetry-only (admin)
iframe.exe list                             # top-level windows
```

## Build

```
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
cargo build --release                                            # x64 app + DLL
cargo build --release --target i686-pc-windows-msvc -p iframe-hook  # x86 DLL
```

Binaries: `target/release/iframe.exe`, `target/release/iframe_hook.dll`,
`target/i686-pc-windows-msvc/release/iframe_hook.dll`.

## Architecture

```
┌──────────────┐  shared memory   ┌─────────────────┐
│  iframe.exe  │◄────────────────►│ iframe_hook.dll │ (injected)
│  UI/profiles │  ring + config   │  JIT pacer      │
│  injector    │                  │  Present hooks  │
│  ETW watch   │                  │  high-res sleep │
└──────────────┘                  └─────────────────┘
```

- `iframe-common` — the pacing math (`JitPacer`: Bresenham cadence on the
  vblank grid, adaptive safety margin, bypass on GPU-bound), the SPSC
  telemetry ring, the runtime config.
- `iframe-hook` — the injected DLL: DXGI Present/Present1 + factory hooks,
  D3D9 chain, the pacing engine, the waitable-timer sleeper.
- `iframe-app` — the control app: UI, injector, profiles, ETW watcher,
  anti-cheat gate.

## Known limitations

- **D3D9**: the hook chain is implemented, but d3d9.dll builds device
  vtables dynamically and reverts patches (an anti-tamper design) —
  sustained D3D9 coverage requires inline export hooks (backlog). DXGI
  (all modern games) is fully supported.
- **ETW mode**: requires an elevated process.
- Kernel-level anti-cheat games are never injected — telemetry-only mode
  only (by design).