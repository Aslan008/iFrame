//! LMTrust test suite for TelemetryTuner.

use iframe_solver::telemetry_tuner::{
    BottleneckDiagnosis, GameTelemetryInput, SideMetrics, TelemetryTuner,
};

#[test]
fn l0_smoke_telemetry_tuner_gpu_bound() {
    let input = GameTelemetryInput {
        off: SideMetrics {
            fps: 58.0,
            p50_ms: 17.2,
            p99_ms: 21.0,
            jitter_ms: 3.8,
            hold_p50_ms: 0.05,
            hold_max_ms: 0.10,
            wait_p50_ms: 0.0,
            late_count: 0,
            sample_count: 500,
        },
        on: SideMetrics::default(),
        refresh_hz: 144.0,
        current_target_fps: 60.0,
    };

    let rec = TelemetryTuner::analyze(&input).expect("Analysis should succeed");
    assert!(rec.diagnoses.contains(&BottleneckDiagnosis::GpuBound));
    assert!(rec.recommended_fps <= 54.0, "Recommended FPS must be below GPU ceiling");
    assert_eq!(rec.recommended_fps, 50.0);
    assert!(rec.expected_latency_reduction_ms > 10.0);
}

#[test]
fn l1_contract_telemetry_tuner_cpu_stutter_mitigation() {
    let input = GameTelemetryInput {
        off: SideMetrics {
            fps: 75.0,
            p50_ms: 13.3,
            p99_ms: 28.0,
            jitter_ms: 14.7, // Heavy GC / Asset loading stutter
            hold_p50_ms: 0.04,
            hold_max_ms: 0.08,
            wait_p50_ms: 0.0,
            late_count: 0,
            sample_count: 600,
        },
        on: SideMetrics::default(),
        refresh_hz: 120.0,
        current_target_fps: 60.0,
    };

    let rec = TelemetryTuner::analyze(&input).expect("Analysis should succeed");
    assert!(rec.diagnoses.contains(&BottleneckDiagnosis::CpuOrEngineStutter));
    assert_eq!(rec.recommended_fps, 72.0);
    assert!(rec.expected_jitter_improvement_times >= 2.0);
}

#[test]
fn l2_boundary_empty_telemetry_returns_error() {
    let input = GameTelemetryInput {
        off: SideMetrics::default(),
        on: SideMetrics::default(),
        refresh_hz: 144.0,
        current_target_fps: 60.0,
    };

    let err = TelemetryTuner::analyze(&input);
    assert!(err.is_err(), "Empty telemetry must return error");
}

#[test]
fn l3_property_expected_metrics_invariants() {
    let input = GameTelemetryInput {
        off: SideMetrics {
            fps: 115.0,
            p50_ms: 8.7,
            p99_ms: 10.2,
            jitter_ms: 1.5,
            hold_p50_ms: 0.03,
            hold_max_ms: 0.06,
            wait_p50_ms: 0.0,
            late_count: 0,
            sample_count: 1000,
        },
        on: SideMetrics::default(),
        refresh_hz: 144.0,
        current_target_fps: 120.0,
    };

    let rec = TelemetryTuner::analyze(&input).expect("Analysis should succeed");
    assert!(rec.expected_latency_reduction_ms >= 5.0);
    assert!(rec.expected_jitter_improvement_times >= 1.0);
    assert!(rec.expected_gpu_load_relief_percent >= 5.0);
    assert!(!rec.cadence_steps.is_empty());
}
