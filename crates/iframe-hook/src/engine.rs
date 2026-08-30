//! The pacing engine: owns the `JitPacer` instance and the high-precision
//! sleeper, lives inside the game process.
//!
//! Hot path contract (see hooks/dxgi.rs):
//! 1. real Present already executed — the frame is on its way to the screen;
//! 2. `pace()` computes the next frame's start time and sleeps;
//! 3. the hook returns — the game starts the next frame with fresh input.
//!
//! The engine is guarded by `try_lock`: if another thread is already pacing
//! (multi-threaded presenters), we simply skip pacing for that frame — never
//! block, never add latency.

use std::sync::{Mutex, OnceLock};

use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::{JitPacer, PacerConfig, PacerDecision, VBlankHint};

use crate::timing::{qpc_frequency, HighResSleeper};

static SLEEPER: OnceLock<HighResSleeper> = OnceLock::new();
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

struct Engine {
    pacer: JitPacer,
    cfg: RuntimeConfig,
    logged: u32,
}

fn pacer_config(cfg: &RuntimeConfig) -> PacerConfig {
    PacerConfig {
        target_fps: if cfg.target_fps > 0.0 { cfg.target_fps } else { 60.0 },
        mode: cfg.mode,
        ..Default::default()
    }
}

/// Run one pacing step. Returns `(release_qpc, decision)` when the frame was
/// paced (the game thread was held until `release_qpc`), or `None` when the
/// limiter stepped aside (disabled, bypass, or estimator cold-start).
pub fn pace(
    t_start: i64,
    t_end: i64,
    cfg: &RuntimeConfig,
    hint: Option<VBlankHint>,
) -> Option<(i64, PacerDecision)> {
    // User refresh override (CLI --refresh): replaces the DWM-reported
    // refresh rate; the vblank PHASE stays on the DWM grid.
    let hint = match hint {
        Some(h) if cfg.refresh_hz > 1.0 => Some(VBlankHint {
            refresh_hz: cfg.refresh_hz,
            ..h
        }),
        other => other,
    };

    let Ok(mut guard) = ENGINE.try_lock() else {
        return None; // another present in flight — never block the hot path
    };
    let engine = guard.get_or_insert_with(|| Engine {
        pacer: JitPacer::new(pacer_config(cfg), qpc_frequency()),
        cfg: *cfg,
        logged: 0,
    });
    if engine.cfg != *cfg {
        engine.pacer.set_config(pacer_config(cfg));
        engine.cfg = *cfg;
    }

    let decision = engine.pacer.on_present_complete(t_start, t_end, hint);
    if decision.stats.bypass {
        // Inert: release immediately, keep the estimator fed.
        engine.pacer.mark_released(t_end);
        return Some((t_end, decision));
    }
    let sleeper = SLEEPER.get_or_init(HighResSleeper::new);
    // Safety cap: a sane pacing wait is < 1 frame interval (≤ 100 ms even at
    // 10 FPS). Anything larger means the schedule is broken — clamp and log.
    sleeper.sleep_until_capped(decision.wake_qpc, 100_000.0);
    let release = crate::timing::qpc_now();
    engine.pacer.mark_released(release);

    // First paced frames: log the schedule for diagnosis.
    const LOG_FRAMES: u32 = 12;
    if engine.logged < LOG_FRAMES {
        engine.logged += 1;
        crate::log_line(&format!(
            "pace #{}/{}: d={:.0}µs ema={:.0}µs margin={:.0}µs sleep={:.0}µs late={} hint={:?}",
            engine.logged,
            LOG_FRAMES,
            decision.stats.frame_duration_us,
            decision.stats.ema_duration_us,
            decision.stats.safety_margin_us,
            decision.stats.sleep_us,
            decision.stats.late,
            hint.map(|h| (h.refresh_hz, h.last_vblank_qpc)),
        ));
    }
    Some((release, decision))
}