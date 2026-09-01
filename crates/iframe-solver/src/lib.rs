//! # iframe-solver: Embedded C++ Physics-CDCL SAT Solver Integration
//!
//! Provides high-performance Boolean constraint solving for:
//! 1. SwapChain / Graphics API / VRR configuration synthesis ([`config_optimizer`]).
//! 2. Optimal discrete VBlank cadence schedule synthesis ([`cadence_synthesizer`]).
//! 3. LMTrust formal property verification.

pub mod cadence_synthesizer;
pub mod config_optimizer;
pub mod telemetry_tuner;

#[repr(C)]
struct OpaqueCdclContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn cdcl_create(num_vars: i32) -> *mut OpaqueCdclContext;
    fn cdcl_add_clause(ctx: *mut OpaqueCdclContext, lits: *const i32, count: i32);
    fn cdcl_set_vsids_mode(ctx: *mut OpaqueCdclContext, enabled: i32);
    fn cdcl_solve(ctx: *mut OpaqueCdclContext, max_conflicts: i32) -> i32;
    fn cdcl_solve_assumptions(
        ctx: *mut OpaqueCdclContext,
        assumptions: *const i32,
        count: i32,
        max_conflicts: i32,
    ) -> i32;
    fn cdcl_get_assignment(ctx: *mut OpaqueCdclContext, var: i32) -> i32;
    fn cdcl_get_conflicts(ctx: *mut OpaqueCdclContext) -> i32;
    fn cdcl_get_propagations(ctx: *mut OpaqueCdclContext) -> i64;
    fn cdcl_destroy(ctx: *mut OpaqueCdclContext);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveResult {
    Sat,
    Unsat,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct SolverStats {
    pub conflicts: i32,
    pub propagations: i64,
}

pub struct CdclSolver {
    raw: *mut OpaqueCdclContext,
}

unsafe impl Send for CdclSolver {}
unsafe impl Sync for CdclSolver {}

impl CdclSolver {
    pub fn new(num_vars: i32) -> Self {
        let raw = unsafe { cdcl_create(num_vars) };
        Self { raw }
    }

    pub fn add_clause(&mut self, lits: &[i32]) {
        if !self.raw.is_null() && !lits.is_empty() {
            unsafe {
                cdcl_add_clause(self.raw, lits.as_ptr(), lits.len() as i32);
            }
        }
    }

    pub fn set_vsids_mode(&mut self, enabled: bool) {
        if !self.raw.is_null() {
            unsafe {
                cdcl_set_vsids_mode(self.raw, if enabled { 1 } else { 0 });
            }
        }
    }

    pub fn solve(&mut self, max_conflicts: i32) -> SolveResult {
        if self.raw.is_null() {
            return SolveResult::Unknown;
        }
        let res = unsafe { cdcl_solve(self.raw, max_conflicts) };
        match res {
            1 => SolveResult::Sat,
            0 => SolveResult::Unsat,
            _ => SolveResult::Unknown,
        }
    }

    pub fn solve_with_assumptions(&mut self, assumptions: &[i32], max_conflicts: i32) -> SolveResult {
        if self.raw.is_null() {
            return SolveResult::Unknown;
        }
        let res = unsafe {
            cdcl_solve_assumptions(
                self.raw,
                assumptions.as_ptr(),
                assumptions.len() as i32,
                max_conflicts,
            )
        };
        match res {
            1 => SolveResult::Sat,
            0 => SolveResult::Unsat,
            _ => SolveResult::Unknown,
        }
    }

    pub fn get_assignment(&self, var: i32) -> Option<bool> {
        if self.raw.is_null() {
            return None;
        }
        let v = unsafe { cdcl_get_assignment(self.raw, var) };
        match v {
            1 => Some(true),
            -1 => Some(false),
            _ => None,
        }
    }

    pub fn stats(&self) -> SolverStats {
        if self.raw.is_null() {
            return SolverStats::default();
        }
        unsafe {
            SolverStats {
                conflicts: cdcl_get_conflicts(self.raw),
                propagations: cdcl_get_propagations(self.raw),
            }
        }
    }
}

impl Drop for CdclSolver {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                cdcl_destroy(self.raw);
            }
            self.raw = std::ptr::null_mut();
        }
    }
}
