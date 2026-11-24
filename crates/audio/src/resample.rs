//! Windowed-sinc interpolation for resampling voices to the device rate.

use std::f64::consts::PI;

/// Taps on each side of the interpolation point.
pub const HALF_TAPS: usize = 16;
/// Sub-sample positions in the table.
const PHASES: usize = 512;

/// A table of windowed-sinc kernels for one cutoff (1.0 = the source's Nyquist frequency).
#[derive(Debug, Clone)]
pub struct SincTable {
    /// `PHASES + 1` rows of `2 * HALF_TAPS` weights.
    weights: Vec<f32>,
    pub cutoff: f32,
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

/// Blackman-Harris window over `[-1, 1]`.
fn window(t: f64) -> f64 {
    if t.abs() >= 1.0 {
        return 0.0;
    }
    let x = (t + 1.0) * 0.5;
    let (a0, a1, a2, a3) = (0.35875, 0.48829, 0.14128, 0.01168);
    a0 - a1 * (2.0 * PI * x).cos() + a2 * (4.0 * PI * x).cos() - a3 * (6.0 * PI * x).cos()
}

impl SincTable {
    /// `cutoff` below 1 low-passes for downsampling (use the ratio of output to input rate).
    pub fn new(cutoff: f32) -> SincTable {
        let cutoff = f64::from(cutoff.clamp(0.05, 1.0));
        let width = 2 * HALF_TAPS;
        let mut weights = vec![0.0f32; (PHASES + 1) * width];
        for phase in 0..=PHASES {
            let frac = phase as f64 / PHASES as f64;
            let row = &mut weights[phase * width..(phase + 1) * width];
            let mut sum = 0.0;
            for (k, w) in row.iter_mut().enumerate() {
                // Tap k sits at source offset k - HALF_TAPS + 1 relative to floor(position).
                let x = (k as f64 - HALF_TAPS as f64 + 1.0) - frac;
                let v = cutoff * sinc(cutoff * x) * window(x / HALF_TAPS as f64);
                *w = v as f32;
                sum += v;
            }
            // Unity gain at DC.
            for w in row.iter_mut() {
                *w = (f64::from(*w) / sum) as f32;
            }
        }
        SincTable { weights, cutoff: cutoff as f32 }
    }

    /// Interpolate at `pos` (source frames) for one channel; `get(i)` returns frame i (zero outside).
    #[inline]
    pub fn interpolate(&self, pos: f64, get: impl Fn(i64) -> f32) -> f32 {
        let base = pos.floor();
        let frac = pos - base;
        let phase = frac * PHASES as f64;
        let p0 = (phase.floor() as usize).min(PHASES - 1);
        let t = (phase - p0 as f64) as f32;
        let width = 2 * HALF_TAPS;
        let r0 = &self.weights[p0 * width..(p0 + 1) * width];
        let r1 = &self.weights[(p0 + 1) * width..(p0 + 2) * width];
        let first = base as i64 - HALF_TAPS as i64 + 1;
        let mut acc = 0.0f32;
        for k in 0..width {
            let w = r0[k] + (r1[k] - r0[k]) * t;
            acc += w * get(first + k as i64);
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_and_exact_positions() {
        let t = SincTable::new(1.0);
        assert!((t.interpolate(10.3, |_| 0.5) - 0.5).abs() < 1e-4);
        // At integer positions the kernel is a unit impulse.
        let sig = |i: i64| (i as f32 * 0.37).sin();
        for i in 20..40 {
            assert!((t.interpolate(i as f64, sig) - sig(i)).abs() < 1e-4);
        }
    }

    #[test]
    fn sine_upsampled_matches_the_ideal_curve() {
        // 1 kHz at 24 kHz, read at 48 kHz positions.
        let t = SincTable::new(1.0);
        let f = 1000.0 / 24000.0;
        let src = |i: i64| (2.0 * std::f64::consts::PI * f * i as f64).sin() as f32;
        let mut worst = 0.0f32;
        for n in 200..2000 {
            let pos = n as f64 * 0.5;
            let ideal = (2.0 * std::f64::consts::PI * f * pos).sin() as f32;
            worst = worst.max((t.interpolate(pos, src) - ideal).abs());
        }
        assert!(worst < 1e-3, "error {worst}");
    }

    #[test]
    fn lowpass_rejects_content_above_the_cutoff() {
        // Downsampling 2:1: a tone at 0.8 of the source Nyquist must be strongly attenuated.
        let t = SincTable::new(0.5);
        let f = 0.4; // Cycles per source sample (0.8 × Nyquist)
        let src = |i: i64| (2.0 * std::f64::consts::PI * f * i as f64).sin() as f32;
        let peak = (100..400).map(|n| t.interpolate(n as f64 * 2.0 + 0.25, src).abs()).fold(0.0, f32::max);
        assert!(peak < 0.05, "leak {peak}");
    }
}
