//! SwapChain, DXGI, and VRR configuration synthesis via CDCL constraint solving.

use iframe_common::config::RuntimeConfig;
use iframe_common::pacer::PacerMode;
use crate::{CdclSolver, SolveResult};

const VAR_ENABLE_PACER: i32 = 1;
const VAR_MODE_VRR: i32 = 2;
const VAR_MODE_FIXED_VSYNC: i32 = 3;
const VAR_MODE_BYPASS: i32 = 4;
const VAR_ALLOW_TEARING: i32 = 5;
const VAR_FORCE_WAITABLE: i32 = 6;
const VAR_VSYNC_OVERRIDE: i32 = 7;
const VAR_FLIP_MODEL: i32 = 8;
const VAR_EXCLUSIVE_FULLSCREEN: i32 = 9;
const VAR_MONITOR_VRR_CAPABLE: i32 = 10;
const VAR_REFRESH_ABOVE_TARGET: i32 = 11;
const TOTAL_VARS: i32 = 11;

#[derive(Debug, Clone)]
pub struct SystemCapabilities {
    pub has_flip_model: bool,
    pub monitor_vrr_capable: bool,
    pub monitor_refresh_hz: f64,
    pub is_exclusive_fullscreen: bool,
}

#[derive(Debug, Clone)]
pub struct UserPreferences {
    pub target_fps: f64,
    pub prefer_vrr: bool,
    pub prefer_lowest_latency: bool,
    pub allow_vsync_override: bool,
}

pub struct ConfigOptimizer;

impl ConfigOptimizer {
    /// Builds the axiomatic DXGI/Windows/GPU compatibility theory in CNF.
    fn build_axioms(solver: &mut CdclSolver) {
        // 1. Exactly one pacer mode: VRR, FixedVsync, or Bypass
        solver.add_clause(&[VAR_MODE_VRR, VAR_MODE_FIXED_VSYNC, VAR_MODE_BYPASS]);
        solver.add_clause(&[-VAR_MODE_VRR, -VAR_MODE_FIXED_VSYNC]);
        solver.add_clause(&[-VAR_MODE_VRR, -VAR_MODE_BYPASS]);
        solver.add_clause(&[-VAR_MODE_FIXED_VSYNC, -VAR_MODE_BYPASS]);

        // 2. Enable pacer <=> not Bypass
        solver.add_clause(&[VAR_ENABLE_PACER, VAR_MODE_BYPASS]);
        solver.add_clause(&[-VAR_ENABLE_PACER, -VAR_MODE_BYPASS]);

        // 3. VRR mode requires VRR capable hardware
        solver.add_clause(&[-VAR_MODE_VRR, VAR_MONITOR_VRR_CAPABLE]);

        // 4. VRR mode requires target FPS <= monitor refresh rate
        solver.add_clause(&[-VAR_MODE_VRR, VAR_REFRESH_ABOVE_TARGET]);

        // 5. Tearing requires DXGI Flip Model
        solver.add_clause(&[-VAR_ALLOW_TEARING, VAR_FLIP_MODEL]);

        // 6. Exclusive Fullscreen conflicts with composition tearing
        solver.add_clause(&[-VAR_EXCLUSIVE_FULLSCREEN, -VAR_ALLOW_TEARING]);

        // 7. Waitable object requires Flip Model
        solver.add_clause(&[-VAR_FORCE_WAITABLE, VAR_FLIP_MODEL]);

        // 8. VSync override with Fixed VSync requires Allow Tearing
        solver.add_clause(&[-VAR_MODE_FIXED_VSYNC, -VAR_VSYNC_OVERRIDE, VAR_ALLOW_TEARING]);
    }

    /// Synthesizes mathematically guaranteed conflict-free runtime configuration.
    pub fn optimize(
        caps: &SystemCapabilities,
        prefs: &UserPreferences,
    ) -> Result<RuntimeConfig, String> {
        let mut solver = CdclSolver::new(TOTAL_VARS);
        solver.set_vsids_mode(true);
        Self::build_axioms(&mut solver);

        // Hardware & Environment facts (Assumptions)
        let mut assumptions = Vec::new();
        assumptions.push(if caps.has_flip_model { VAR_FLIP_MODEL } else { -VAR_FLIP_MODEL });
        assumptions.push(if caps.is_exclusive_fullscreen {
            VAR_EXCLUSIVE_FULLSCREEN
        } else {
            -VAR_EXCLUSIVE_FULLSCREEN
        });
        assumptions.push(if caps.monitor_vrr_capable {
            VAR_MONITOR_VRR_CAPABLE
        } else {
            -VAR_MONITOR_VRR_CAPABLE
        });

        let refresh_above = caps.monitor_refresh_hz >= prefs.target_fps && prefs.target_fps > 0.0;
        assumptions.push(if refresh_above {
            VAR_REFRESH_ABOVE_TARGET
        } else {
            -VAR_REFRESH_ABOVE_TARGET
        });

        // User preference goals (soft constraints prioritized via assumptions)
        if prefs.prefer_vrr && caps.monitor_vrr_capable && refresh_above {
            assumptions.push(VAR_MODE_VRR);
        } else if prefs.target_fps > 0.0 {
            assumptions.push(VAR_MODE_FIXED_VSYNC);
        } else {
            assumptions.push(VAR_MODE_BYPASS);
        }

        if prefs.prefer_lowest_latency && caps.has_flip_model && !caps.is_exclusive_fullscreen {
            assumptions.push(VAR_ALLOW_TEARING);
        }

        if prefs.allow_vsync_override {
            assumptions.push(VAR_VSYNC_OVERRIDE);
        }

        let mut res = solver.solve_with_assumptions(&assumptions, 10000);
        if res != SolveResult::Sat {
            // If user's soft preferences are UNSAT on this hardware, solve with pure hardware facts
            let hw_assumptions: Vec<i32> = assumptions[..4].to_vec();
            res = solver.solve_with_assumptions(&hw_assumptions, 10000);
            if res != SolveResult::Sat {
                return Err("Failed to synthesize valid graphics configuration (UNSAT)".into());
            }
        }

        let is_vrr = solver.get_assignment(VAR_MODE_VRR).unwrap_or(false);
        let is_fixed = solver.get_assignment(VAR_MODE_FIXED_VSYNC).unwrap_or(false);
        let mode = if is_vrr {
            PacerMode::Vrr
        } else if is_fixed {
            PacerMode::FixedVsync
        } else {
            PacerMode::Bypass
        };

        let enabled = solver.get_assignment(VAR_ENABLE_PACER).unwrap_or(false);
        let vsync_override = solver.get_assignment(VAR_VSYNC_OVERRIDE).unwrap_or(false);
        let force_waitable = solver.get_assignment(VAR_FORCE_WAITABLE).unwrap_or(false);

        Ok(RuntimeConfig {
            enabled,
            mode,
            target_fps: prefs.target_fps,
            refresh_hz: caps.monitor_refresh_hz,
            vsync_override,
            force_waitable,
            reflex_mode: Default::default(),
            overlay_enabled: false,
        })
    }
}
