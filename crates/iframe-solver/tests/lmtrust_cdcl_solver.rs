//! LMTrust formal verification test suite for C++ CDCL Solver Integration.

use iframe_solver::cadence_synthesizer::CadenceSynthesizer;
use iframe_solver::config_optimizer::{ConfigOptimizer, SystemCapabilities, UserPreferences};
use iframe_solver::{CdclSolver, SolveResult};

#[test]
fn l0_smoke_solver_creation_and_solve_simple_sat() {
    let mut solver = CdclSolver::new(3);
    // (x1 ∨ x2) ∧ (¬x1 ∨ x3) ∧ (¬x2 ∨ ¬x3)
    solver.add_clause(&[1, 2]);
    solver.add_clause(&[-1, 3]);
    solver.add_clause(&[-2, -3]);

    let res = solver.solve(1000);
    assert_eq!(res, SolveResult::Sat);

    let x1 = solver.get_assignment(1);
    let x2 = solver.get_assignment(2);
    let x3 = solver.get_assignment(3);
    assert!(x1.is_some() && x2.is_some() && x3.is_some());
}

#[test]
fn l1_contract_simple_unsat_proof() {
    let mut solver = CdclSolver::new(2);
    // (x1) ∧ (¬x1)
    solver.add_clause(&[1]);
    solver.add_clause(&[-1]);

    let res = solver.solve(1000);
    assert_eq!(res, SolveResult::Unsat);
}

#[test]
fn l2_boundary_pigeonhole_principle_3_pigeons_2_holes() {
    // 3 pigeons, 2 holes (variables: p_ij = pigeon i in hole j)
    // p1: 1, 2; p2: 3, 4; p3: 5, 6
    let mut solver = CdclSolver::new(6);
    solver.set_vsids_mode(true);

    // Each pigeon in at least one hole:
    solver.add_clause(&[1, 2]);
    solver.add_clause(&[3, 4]);
    solver.add_clause(&[5, 6]);

    // No hole has >1 pigeon:
    // Hole 1: (¬1 ∨ ¬3), (¬1 ∨ ¬5), (¬3 ∨ ¬5)
    solver.add_clause(&[-1, -3]);
    solver.add_clause(&[-1, -5]);
    solver.add_clause(&[-3, -5]);

    // Hole 2: (¬2 ∨ ¬4), (¬2 ∨ ¬6), (¬4 ∨ ¬6)
    solver.add_clause(&[-2, -4]);
    solver.add_clause(&[-2, -6]);
    solver.add_clause(&[-4, -6]);

    let res = solver.solve(5000);
    assert_eq!(res, SolveResult::Unsat, "PHP(3,2) must be proved UNSAT");
}

#[test]
fn l3_property_config_optimizer_synthesis() {
    let caps = SystemCapabilities {
        has_flip_model: true,
        monitor_vrr_capable: true,
        monitor_refresh_hz: 144.0,
        is_exclusive_fullscreen: false,
    };
    let prefs = UserPreferences {
        target_fps: 120.0,
        prefer_vrr: true,
        prefer_lowest_latency: true,
        allow_vsync_override: false,
    };

    let config = ConfigOptimizer::optimize(&caps, &prefs).expect("Synthesis should succeed");
    assert!(config.enabled);
    assert_eq!(config.mode, iframe_common::pacer::PacerMode::Vrr);
    assert_eq!(config.target_fps, 120.0);
}

#[test]
fn l4_adversarial_cadence_synthesizer_all_standard_rates() {
    let cases = [
        (50.0, 120.0), // 50 FPS @ 120 Hz -> ratio 12/5 = 2.4 vblanks
        (40.0, 120.0), // 40 FPS @ 120 Hz -> ratio 3/1 = 3.0 vblanks
        (60.0, 144.0), // 60 FPS @ 144 Hz -> ratio 12/5 = 2.4 vblanks
        (45.0, 60.0),  // 45 FPS @ 60 Hz -> ratio 4/3 = 1.33 vblanks
    ];

    for (fps, hz) in cases {
        let schedule = CadenceSynthesizer::synthesize(fps, hz)
            .expect(&format!("Synthesis failed for {fps} FPS @ {hz} Hz"));

        assert_eq!(schedule.steps.len(), schedule.period_frames);
        let total_vblanks: usize = schedule.steps.iter().sum();
        assert_eq!(total_vblanks, schedule.total_vblanks);

        // Check max jitter invariant: adjacent steps differ by at most 1
        for i in 0..schedule.steps.len() {
            let next = (i + 1) % schedule.steps.len();
            let diff = (schedule.steps[i] as isize - schedule.steps[next] as isize).abs();
            assert!(diff <= 1, "Cadence jitter exceeded at step {i}");
        }
    }
}
