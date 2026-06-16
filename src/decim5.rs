use num_complex::Complex32;
use std::f32::consts::PI;

const DECIM: usize = 5;
const TAPS_PER_PHASE: usize = 16;
const NUM_TAPS: usize = DECIM * TAPS_PER_PHASE + 1;

// Cutoff in cycles/sample, where 0.5 is input Nyquist.
//
// For decimation by 5, output Nyquist corresponds to 0.5 / 5 = 0.1.
// Use slightly below that to leave transition bandwidth.
const CUTOFF: f32 = 0.45 / DECIM as f32;

fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-8 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn design_decim5_lowpass() -> [f32; NUM_TAPS] {
    let mut h = [0.0f32; NUM_TAPS];
    let mid = NUM_TAPS / 2;

    for (i, tap) in h.iter_mut().enumerate() {
        let x = i.cast_signed() - mid.cast_signed();
        let x = x as f32;

        // Ideal lowpass impulse response.
        let ideal = 2.0 * CUTOFF * sinc(2.0 * CUTOFF * x);

        // Hamming window.
        let window = 0.54 - 0.46 * (2.0 * PI * i as f32 / (NUM_TAPS as f32 - 1.0)).cos();

        *tap = ideal * window;
    }

    // Normalize DC gain to 1.
    let sum: f32 = h.iter().sum();
    for v in &mut h {
        *v /= sum;
    }

    h
}

/// Lowpass filter + decimate by 5.
///
/// Computes only output samples.
/// Uses zero-padding at the boundaries.
/// Group delay is `(NUM_TAPS - 1) / 2 = 40` input samples,
/// or 8 output samples.
pub fn decim5(input: &[Complex32]) -> Vec<Complex32> {
    let taps = design_decim5_lowpass();

    let delay = NUM_TAPS / 2;
    let out_len = input.len().div_ceil(DECIM);
    let mut out = Vec::with_capacity(out_len);

    for out_idx in 0..out_len {
        let center = out_idx * DECIM;
        let mut acc = Complex32::new(0.0, 0.0);

        for (k, tap) in taps.iter().enumerate() {
            let input_idx = center.cast_signed() + k.cast_signed() - delay.cast_signed();

            if input_idx >= 0 {
                let input_idx = input_idx as usize;
                if input_idx < input.len() {
                    acc += input[input_idx] * *tap;
                }
            }
        }

        out.push(acc);
    }

    out
}
