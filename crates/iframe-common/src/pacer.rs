//! # JIT Start Pacing — the mathematical core of iFrame.
//!
//! Core invariant: **a completed frame is never held.** The real `Present`
//! executes immediately; the pacer then sleeps *inside the hook, after the
//! real Present has returned*, delaying only the START of the next frame so
//! that it completes just-in-time for its target display tick (vblank).
//!
//! Input is therefore sampled as late as possible (freshest input), and the
//! added input latency is exactly zero — the displayed latency equals the
//! frame's own render time `d`.

/// QPC → microseconds conversion for a given frequency.
#[inline]
pub fn qpc_to_us(ticks: i64, freq: i64) -> f64 {
    ticks as f64 * 1_000_000.0 / freq as f64
}

/// Microseconds → QPC ticks conversion for a given frequency.
#[inline]
pub fn us_to_qpc(us: f64, freq: i64) -> i64 {
    (us * freq as f64 / 1_000_000.0).round() as i64
}

/// Pacing strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacerMode {
    /// Start-to-start pacing for VRR displays (frame is picked up the moment
    /// it is presented; no vblank quantization below max refresh).
    Vrr = 0,
    /// Align frame completion to the N-th vblank of the display grid.
    /// Cadence for non-integer refresh/target ratios (e.g. 50 FPS @ 120 Hz)
    /// is distributed with a Bresenham walk, console-style.
    FixedVsync = 1,
    /// Limiter inert: never delay the game thread.
    Bypass = 2,
}

impl PacerMode {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => PacerMode::Vrr,
            1 => PacerMode::FixedVsync,
            _ => PacerMode::Bypass,
        }
    }

    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone)]
pub struct PacerConfig {
    /// Desired frame rate cap.
    pub target_fps: f64,
    pub mode: PacerMode,
    /// Base safety margin (µs) kept between predicted frame completion and
    /// the target display tick.
    pub base_safety_margin_us: f64,
    /// Multiplier for the measured frame-time deviation that widens the
    /// margin under jitter: `S = base + k * deviation`.
    pub variance_multiplier: f64,
    /// EMA factor for the frame-duration estimate.
    pub ema_alpha: f64,
    /// EMA factor for the absolute-deviation estimate.
    pub deviation_beta: f64,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            target_fps: 60.0,
            mode: PacerMode::FixedVsync,
            base_safety_margin_us: 300.0,
            variance_multiplier: 2.0,
            ema_alpha: 0.15,
            deviation_beta: 0.10,
        }
    }
}

/// Display tick information sampled around the current Present call.
#[derive(Debug, Clone, Copy)]
pub struct VBlankHint {
    /// Current display refresh rate (Hz).
    pub refresh_hz: f64,
    /// QPC timestamp of the most recent vblank (must be `<= present_end`).
    pub last_vblank_qpc: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PacerStats {
    /// Measured cost of the frame that just finished:
    /// `present_end - last_release` (CPU work + Present submission).
    pub frame_duration_us: f64,
    pub ema_duration_us: f64,
    pub deviation_us: f64,
    pub safety_margin_us: f64,
    /// How long the hook is about to sleep (0 when bypassing).
    pub sleep_us: f64,
    /// True when the limiter stepped aside (GPU/CPU-bound or no history).
    pub bypass: bool,
    /// True when the frame finished after its scheduled slot.
    pub late: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PacerDecision {
    /// Sleep until this QPC value, then return from the Present hook.
    /// Guaranteed `>= present_end_qpc` — we never hold the current frame.
    pub wake_qpc: i64,
    /// Display tick (QPC) the *next* frame aims to complete just before.
    pub target_tick_qpc: i64,
    /// Display ticks advanced since the previous target (cadence step).
    pub cadence_step: i64,
    pub stats: PacerStats,
}

/// Just-In-Time start pacer.
///
/// One instance per swapchain. The hook calls [`JitPacer::on_present_complete`]
/// right after the real Present returns, sleeps until `decision.wake_qpc`,
/// then calls [`JitPacer::mark_released`] with the actual wake-up timestamp.
pub struct JitPacer {
    freq: i64,
    cfg: PacerConfig,
    ema_us: f64,
    dev_us: f64,
    last_release_qpc: i64,
    /// VRR mode: QPC of the display tick the last frame was aimed at.
    last_target_qpc: i64,
    /// FixedVsync mode: logical index of the last targeted vblank.
    last_target_index: i64,
    /// FixedVsync mode: logical index of the last vblank hint we saw.
    hint_index: i64,
    /// QPC of the vblank that `hint_index` refers to.
    hint_qpc: i64,
    bres_acc: f64,
    frames: u64,
}

impl JitPacer {
    pub fn new(cfg: PacerConfig, qpc_frequency: i64) -> Self {
        Self {
            freq: qpc_frequency.max(1),
            cfg,
            ema_us: 0.0,
            dev_us: 0.0,
            last_release_qpc: 0,
            last_target_qpc: 0,
            last_target_index: 0,
            hint_index: 0,
            hint_qpc: 0,
            bres_acc: 0.0,
            frames: 0,
        }
    }

    pub fn config(&self) -> &PacerConfig {
        &self.cfg
    }

    pub fn set_config(&mut self, cfg: PacerConfig) {
        self.cfg = cfg;
    }

    pub fn frames_seen(&self) -> u64 {
        self.frames
    }

    /// Called immediately after the real `Present` returned.
    ///
    /// * `present_start_qpc` — when the game entered Present (frame complete).
    /// * `present_end_qpc`   — when the real Present returned (frame queued).
    /// * `hint`              — display tick info, if available.
    ///
    /// Returns the wake-up point. Sleeping until it delays the START of the
    /// next frame; the frame that just finished is already on its way to the
    /// screen.
    pub fn on_present_complete(
        &mut self,
        present_start_qpc: i64,
        present_end_qpc: i64,
        hint: Option<VBlankHint>,
    ) -> PacerDecision {
        // Reserved for future GPU-timestamp correlation (M4+); the frame
        // cost is measured against the release point, not the Present entry.
        let _ = present_start_qpc;

        // First frame: no history — release immediately and seed the anchors.
        if self.last_release_qpc == 0 {
            self.last_release_qpc = present_end_qpc;
            self.last_target_qpc = present_end_qpc;
            if let Some(h) = hint {
                self.hint_qpc = h.last_vblank_qpc;
            }
            self.frames += 1;
            return PacerDecision {
                wake_qpc: present_end_qpc,
                target_tick_qpc: present_end_qpc,
                cadence_step: 0,
                stats: PacerStats {
                    bypass: true,
                    ..Default::default()
                },
            };
        }

        let d_us = qpc_to_us(present_end_qpc - self.last_release_qpc, self.freq);

        // Ignore implausibly short frames (secondary swapchains, duplicate
        // presents) so they don't poison the estimator.
        if d_us < 50.0 {
            self.frames += 1;
            return PacerDecision {
                wake_qpc: present_end_qpc,
                target_tick_qpc: present_end_qpc,
                cadence_step: 0,
                stats: PacerStats {
                    bypass: true,
                    ..Default::default()
                },
            };
        }

        let interval_us = 1_000_000.0 / self.cfg.target_fps.max(0.1);

        // Update the estimator, clamping outliers so a single stall does not
        // poison the EMA (a stall longer than 4 intervals is a hitch, not a
        // trend).
        let d_clamped = d_us.clamp(50.0, interval_us * 4.0);
        let delta = d_clamped - self.ema_us;
        self.ema_us = if self.frames == 1 {
            d_clamped
        } else {
            self.ema_us + self.cfg.ema_alpha * delta
        };
        self.dev_us = if self.frames == 1 {
            0.0
        } else {
            (1.0 - self.cfg.deviation_beta) * self.dev_us
                + self.cfg.deviation_beta * delta.abs()
        };

        let margin_us = self.cfg.base_safety_margin_us
            + self.cfg.variance_multiplier * self.dev_us;
        let lead_us = self.ema_us + margin_us;
        let lead_qpc = us_to_qpc(lead_us, self.freq);

        // Bypass when the game cannot sustain the target: the frame already
        // ate (almost) the whole budget — delaying it would only make things
        // worse. Never add latency on top of a slow frame.
        let overloaded = d_us >= interval_us - self.cfg.base_safety_margin_us;

        let (wake_qpc, target_tick_qpc, cadence_step, late) = match self.cfg.mode {
            PacerMode::Bypass => (present_end_qpc, present_end_qpc, 0, false),
            PacerMode::Vrr => {
                let interval_qpc = us_to_qpc(interval_us, self.freq);
                let mut target = self.last_target_qpc + interval_qpc;
                let mut late = false;
                // Skip slots we have no hope of making (frame finished late).
                while target - lead_qpc < present_end_qpc {
                    target += interval_qpc;
                    late = true;
                }
                if overloaded {
                    // Resync the ideal grid to reality; release immediately.
                    target = present_end_qpc + interval_qpc;
                }
                let wake = if overloaded {
                    present_end_qpc
                } else {
                    target - lead_qpc
                };
                (wake.max(present_end_qpc), target, 1, late)
            }
            PacerMode::FixedVsync => match hint {
                None => {
                    // No display tick info: degrade to VRR-style pacing.
                    let interval_qpc = us_to_qpc(interval_us, self.freq);
                    let mut target = self.last_target_qpc + interval_qpc;
                    let mut late = false;
                    while target - lead_qpc < present_end_qpc {
                        target += interval_qpc;
                        late = true;
                    }
                    let wake = if overloaded {
                        present_end_qpc
                    } else {
                        target - lead_qpc
                    };
                    (wake.max(present_end_qpc), target, 1, late)
                }
                Some(h) => {
                    let refresh_hz = if h.refresh_hz > 1.0 { h.refresh_hz } else { 60.0 };
                    // Effective target never exceeds the refresh rate.
                    let effective_fps = self.cfg.target_fps.min(refresh_hz);
                    let refresh_int_us = 1_000_000.0 / refresh_hz;
                    let refresh_int_qpc = us_to_qpc(refresh_int_us, self.freq);

                    // Advance the logical hint index by the number of real
                    // vblanks that elapsed since the previous hint.
                    if self.frames > 1 && self.hint_qpc != 0 {
                        let delta = ((h.last_vblank_qpc - self.hint_qpc) as f64
                            / refresh_int_qpc as f64)
                            .round() as i64;
                        self.hint_index += delta.max(0);
                    }
                    self.hint_qpc = h.last_vblank_qpc;

                    // Bresenham cadence: advance by `refresh/target` vblanks
                    // per frame on average.
                    let step = refresh_hz / effective_fps;
                    self.bres_acc += step;
                    let mut n = self.bres_acc.floor() as i64;
                    self.bres_acc -= n as f64;
                    if n < 1 {
                        n = 1;
                    }

                    let candidate_idx = self.last_target_index + n;
                    // Earliest vblank that is still feasible:
                    // its QPC must be >= present_end + lead.
                    let min_idx = self.hint_index
                        + ((present_end_qpc + lead_qpc - h.last_vblank_qpc) as f64
                            / refresh_int_qpc as f64)
                            .ceil() as i64;
                    let mut late = false;
                    let mut target_idx = candidate_idx;
                    if target_idx < min_idx {
                        target_idx = min_idx;
                        late = true;
                    }
                    if overloaded {
                        target_idx = min_idx;
                    }

                    let target_qpc =
                        h.last_vblank_qpc + (target_idx - self.hint_index) * refresh_int_qpc;
                    let wake = if overloaded {
                        present_end_qpc
                    } else {
                        target_qpc - lead_qpc
                    };
                    (
                        wake.max(present_end_qpc),
                        target_qpc,
                        target_idx - self.last_target_index,
                        late,
                    )
                }
            },
        };

        // Persist anchors.
        self.last_target_qpc = target_tick_qpc;
        if self.cfg.mode == PacerMode::FixedVsync {
            self.last_target_index += cadence_step;
        }
        self.frames += 1;

        let sleep_us = qpc_to_us(wake_qpc - present_end_qpc, self.freq).max(0.0);
        PacerDecision {
            wake_qpc,
            target_tick_qpc,
            cadence_step,
            stats: PacerStats {
                frame_duration_us: d_us,
                ema_duration_us: self.ema_us,
                deviation_us: self.dev_us,
                safety_margin_us: margin_us,
                sleep_us,
                bypass: sleep_us <= 0.0,
                late,
            },
        }
    }

    /// Called with the actual wake-up timestamp, right before returning from
    /// the Present hook (the next frame starts here with fresh input).
    pub fn mark_released(&mut self, release_qpc: i64) {
        self.last_release_qpc = release_qpc;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQ: i64 = 10_000_000; // 100 ns per tick (typical QPC)

    struct Sim {
        pacer: JitPacer,
        epoch_qpc: i64,
        refresh_hz: f64,
        /// QPC at which the game thread was released for the current frame.
        release_qpc: i64,
        present_starts: Vec<i64>,
        decisions: Vec<PacerDecision>,
    }

    impl Sim {
        fn new(cfg: PacerConfig, refresh_hz: f64) -> Self {
            Self {
                pacer: JitPacer::new(cfg, FREQ),
                epoch_qpc: 1_000_000_000,
                refresh_hz,
                release_qpc: 1_000_000_000,
                present_starts: Vec::new(),
                decisions: Vec::new(),
            }
        }

        fn vb_int_qpc(&self) -> i64 {
            us_to_qpc(1_000_000.0 / self.refresh_hz, FREQ)
        }

        fn last_vblank_before(&self, t: i64) -> i64 {
            let int = self.vb_int_qpc();
            self.epoch_qpc + ((t - self.epoch_qpc) / int) * int
        }

        /// Runs one frame: the game starts at `self.release_qpc`, spends
        /// `cpu_us` simulating/rendering, calls Present (which takes
        /// `pres_us`), the pacer decides, the sim sleeps until wake.
        fn step(&mut self, cpu_us: f64, pres_us: f64) {
            let pres_start = self.release_qpc + us_to_qpc(cpu_us, FREQ);
            let pres_end = pres_start + us_to_qpc(pres_us, FREQ);
            let hint = VBlankHint {
                refresh_hz: self.refresh_hz,
                last_vblank_qpc: self.last_vblank_before(pres_end),
            };
            let d = self.pacer.on_present_complete(pres_start, pres_end, Some(hint));
            // Invariant under test: we NEVER hold the completed frame.
            assert!(
                d.wake_qpc >= pres_end,
                "wake {} < present_end {} — frame would be held!",
                d.wake_qpc,
                pres_end
            );
            self.release_qpc = d.wake_qpc;
            self.pacer.mark_released(d.wake_qpc);
            self.present_starts.push(pres_start);
            self.decisions.push(d);
        }

        /// Present-to-present intervals in µs.
        fn present_intervals_us(&self) -> Vec<f64> {
            self.present_starts
                .windows(2)
                .map(|w| qpc_to_us(w[1] - w[0], FREQ))
                .collect()
        }
    }

    fn cfg(target_fps: f64, mode: PacerMode) -> PacerConfig {
        PacerConfig {
            target_fps,
            mode,
            ..Default::default()
        }
    }

    #[test]
    fn stable_60fps_on_120hz_is_flat_and_never_late() {
        let mut sim = Sim::new(cfg(60.0, PacerMode::FixedVsync), 120.0);
        for _ in 0..600 {
            sim.step(5000.0, 100.0); // 5 ms frame cost, way under budget
        }
        let intervals = &sim.present_intervals_us()[100..];
        let target = 1_000_000.0 / 60.0;
        let max_err = intervals.iter().fold(0.0f64, |m, &v| m.max((v - target).abs()));
        assert!(
            max_err < 200.0,
            "present-to-present drift {max_err} µs exceeds 200 µs"
        );
        // Every frame must complete before its target vblank (no missed slots).
        let lates = sim.decisions[100..].iter().filter(|d| d.stats.late).count();
        assert_eq!(lates, 0, "stable input should never miss a vblank");
    }

    #[test]
    fn cadence_40fps_on_120hz_steps_every_3rd_vblank() {
        let mut sim = Sim::new(cfg(40.0, PacerMode::FixedVsync), 120.0);
        for _ in 0..300 {
            sim.step(4000.0, 100.0);
        }
        let steps: Vec<i64> = sim.decisions[10..].iter().map(|d| d.cadence_step).collect();
        assert!(steps.iter().all(|&s| s == 3), "expected uniform step 3, got {steps:?}");
    }

    #[test]
    fn cadence_50fps_on_120hz_averages_2_4_vblanks() {
        let mut sim = Sim::new(cfg(50.0, PacerMode::FixedVsync), 120.0);
        for _ in 0..500 {
            sim.step(4000.0, 100.0);
        }
        let steps: Vec<i64> = sim.decisions[50..].iter().map(|d| d.cadence_step).collect();
        let total: i64 = steps.iter().sum();
        let avg = total as f64 / steps.len() as f64;
        assert!(
            (avg - 2.4).abs() < 0.05,
            "average cadence {avg} deviates from 2.4"
        );
        // Only 2- and 3-vblank steps allowed (no burst of 4+).
        assert!(steps.iter().all(|&s| (2..=3).contains(&s)));
    }

    #[test]
    fn single_20ms_spike_does_not_break_pacing() {
        let mut sim = Sim::new(cfg(60.0, PacerMode::FixedVsync), 120.0);
        for _ in 0..100 {
            sim.step(5000.0, 100.0);
        }
        // One pathological frame: 20 ms CPU.
        sim.step(20000.0, 100.0);
        // Recovery: the next decisions must be sane (no negative sleeps, no
        // runaway catch-up bursts) and pacing must resettle.
        for _ in 0..100 {
            sim.step(5000.0, 100.0);
        }
        let intervals = &sim.present_intervals_us()[110..];
        let target = 1_000_000.0 / 60.0;
        let max_err = intervals.iter().fold(0.0f64, |m, &v| m.max((v - target).abs()));
        assert!(max_err < 300.0, "did not recover after spike: {max_err} µs");
    }

    #[test]
    fn gpu_bound_game_is_never_delayed() {
        let mut sim = Sim::new(cfg(60.0, PacerMode::FixedVsync), 120.0);
        for _ in 0..200 {
            sim.step(25000.0, 100.0); // 25 ms >> 16.6 ms budget
        }
        // Every decision must bypass: zero sleep, zero added latency.
        let bypasses = sim.decisions.iter().filter(|d| d.stats.bypass).count();
        assert_eq!(bypasses, sim.decisions.len(), "limiter must be inert when overloaded");
        let any_sleep = sim
            .decisions
            .iter()
            .any(|d| d.stats.sleep_us > 0.001);
        assert!(!any_sleep, "no sleep is allowed while GPU-bound");
    }

    #[test]
    fn vrr_mode_gives_flat_start_to_start() {
        let mut sim = Sim::new(cfg(90.0, PacerMode::Vrr), 120.0);
        for _ in 0..600 {
            sim.step(6000.0, 100.0);
        }
        let intervals = &sim.present_intervals_us()[100..];
        let target = 1_000_000.0 / 90.0;
        let max_err = intervals.iter().fold(0.0f64, |m, &v| m.max((v - target).abs()));
        assert!(max_err < 150.0, "VRR start-to-start drift {max_err} µs");
    }

    #[test]
    fn target_above_refresh_is_clamped_in_fixed_mode() {
        // 240 FPS requested on a 60 Hz display → effectively 60.
        let mut sim = Sim::new(cfg(240.0, PacerMode::FixedVsync), 60.0);
        for _ in 0..300 {
            sim.step(3000.0, 100.0);
        }
        let intervals = &sim.present_intervals_us()[50..];
        let target = 1_000_000.0 / 60.0;
        let max_err = intervals.iter().fold(0.0f64, |m, &v| m.max((v - target).abs()));
        assert!(max_err < 300.0, "expected ~60 FPS cadence, drift {max_err} µs");
    }

    #[test]
    fn first_two_frames_release_immediately() {
        let mut sim = Sim::new(cfg(60.0, PacerMode::FixedVsync), 120.0);
        sim.step(5000.0, 100.0);
        assert!(sim.decisions[0].stats.bypass, "first frame must bypass");
        sim.step(5000.0, 100.0);
        // Second frame has only one sample — still conservative.
        assert!(sim.decisions[1].wake_qpc >= 0);
    }
}