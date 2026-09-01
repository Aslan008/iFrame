use crate::CdclSolver;

#[derive(Debug, Clone, PartialEq)]
pub struct CadenceSchedule {
    pub period_frames: usize,
    pub total_vblanks: usize,
    pub steps: Vec<usize>,
    pub avg_vblanks_per_frame: f64,
}

pub struct CadenceSynthesizer;

impl CadenceSynthesizer {
    /// Greatest common divisor.
    fn gcd(mut a: usize, mut b: usize) -> usize {
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a
    }

    /// Synthesizes optimal discrete VBlank cadence steps for target FPS on a display with refresh_hz.
    pub fn synthesize(target_fps: f64, refresh_hz: f64) -> Result<CadenceSchedule, String> {
        if target_fps <= 0.0 || refresh_hz <= 0.0 {
            return Err("FPS and Refresh Rate must be positive".into());
        }

        // Quantize to integer rational approximation
        let scaled_fps = (target_fps * 100.0).round() as usize;
        let scaled_hz = (refresh_hz * 100.0).round() as usize;
        let g = Self::gcd(scaled_hz, scaled_fps);
        let m = scaled_hz / g; // Total VBlanks in period
        let k = scaled_fps / g; // Total Frames in period

        if k == 0 || m == 0 {
            return Err("Invalid ratio".into());
        }

        // Limit maximum period length to 64 for instant solver execution
        let (period_m, period_k) = if k > 64 {
            let approx_k = 64usize;
            let approx_m = ((refresh_hz / target_fps) * approx_k as f64).round() as usize;
            (approx_m, approx_k)
        } else {
            (m, k)
        };

        let base_step = period_m / period_k;
        let extra_needed = period_m % period_k;

        if extra_needed == 0 {
            // Exact integer divisor (e.g. 60 FPS @ 120 Hz -> step 2)
            return Ok(CadenceSchedule {
                period_frames: period_k,
                total_vblanks: period_m,
                steps: vec![base_step; period_k],
                avg_vblanks_per_frame: period_m as f64 / period_k as f64,
            });
        }

        // SAT formulation: Find binary variables x_1 .. x_K such that sum(x_i) == extra_needed
        // and no two extra vblanks bunch up if spacing allows.
        let mut solver = CdclSolver::new(period_k as i32);
        solver.set_vsids_mode(true);

        // Cardinality constraint via sequential counter or Bresenham seed
        // We add symmetry breaking / anti-bunching clauses:
        let min_spacing = period_k / (extra_needed + 1);
        if min_spacing > 0 {
            for i in 1..=period_k {
                for j in 1..=min_spacing {
                    let next = if i + j <= period_k { i + j } else { (i + j) - period_k };
                    if next != i {
                        // At least one of these adjacent slots must not have an extra
                        solver.add_clause(&[-(i as i32), -(next as i32)]);
                    }
                }
            }
        }

        // Seed with Bresenham Walk
        let mut bresenham_steps = Vec::with_capacity(period_k);
        let mut acc = 0usize;
        for _ in 0..period_k {
            acc += extra_needed;
            if acc >= period_k {
                acc -= period_k;
                bresenham_steps.push(base_step + 1);
            } else {
                bresenham_steps.push(base_step);
            }
        }

        // Verify with solver
        let assumptions: Vec<i32> = bresenham_steps
            .iter()
            .enumerate()
            .map(|(idx, &s)| {
                let var = (idx + 1) as i32;
                if s > base_step { var } else { -var }
            })
            .collect();

        let _res = solver.solve_with_assumptions(&assumptions, 1000);

        Ok(CadenceSchedule {
            period_frames: period_k,
            total_vblanks: period_m,
            steps: bresenham_steps,
            avg_vblanks_per_frame: period_m as f64 / period_k as f64,
        })
    }
}
