//! Polyphase windowed-sinc fractional resampler.
//!
//! This kernel is intentionally policy-free: it knows nothing about FIFO
//! occupancy, UDP, WASAPI, or clock control. It only evaluates one stereo
//! sample at a fractional source position.

use crate::plane::l1_media::pcm::ring::SourcePcmRing;

pub const DEFAULT_TAPS: usize = 63;
pub const DEFAULT_PHASES: usize = 2048;
pub const DEFAULT_CUTOFF: f64 = 0.94;
const KAISER_BETA: f64 = 8.6;

#[derive(Debug)]
pub struct PolyphaseSinc {
    taps: usize,
    phases: usize,
    left_taps: usize,
    table: Box<[f32]>,
}

impl PolyphaseSinc {
    pub fn reference() -> Self {
        Self::new(DEFAULT_TAPS, DEFAULT_PHASES, DEFAULT_CUTOFF)
            .expect("reference ASRC parameters are valid")
    }

    pub fn new(taps: usize, phases: usize, cutoff: f64) -> Result<Self, &'static str> {
        if taps < 7 || taps % 2 == 0 {
            return Err("ASRC taps must be odd and >= 7");
        }
        if phases < 16 {
            return Err("ASRC phases must be >= 16");
        }
        if !(0.5..=1.0).contains(&cutoff) {
            return Err("ASRC cutoff must be in 0.5..=1.0");
        }

        let left_taps = taps / 2;
        let mut table = Vec::with_capacity((phases + 1) * taps);

        for phase_index in 0..=phases {
            let frac = phase_index as f64 / phases as f64;
            let mut phase = Vec::with_capacity(taps);
            let mut sum = 0.0f64;

            for tap in 0..taps {
                let x = tap as f64 - left_taps as f64 - frac;
                let ideal = cutoff * sinc_pi(cutoff * x);
                let window = kaiser_window(tap, taps, KAISER_BETA);
                let coefficient = ideal * window;
                phase.push(coefficient);
                sum += coefficient;
            }

            for coefficient in phase {
                table.push((coefficient / sum) as f32);
            }
        }

        Ok(Self {
            taps,
            phases,
            left_taps,
            table: table.into_boxed_slice(),
        })
    }

    pub fn taps(&self) -> usize {
        self.taps
    }

    pub fn left_taps(&self) -> usize {
        self.left_taps
    }

    pub fn sample(
        &self,
        ring: &SourcePcmRing,
        source_position: f64,
    ) -> Option<[f32; 2]> {
        if !source_position.is_finite() || source_position < self.left_taps as f64 {
            return None;
        }

        let integer = source_position.floor() as usize;
        let frac = source_position - integer as f64;
        let start = integer.checked_sub(self.left_taps)?;

        if start.checked_add(self.taps)? > ring.available_frames() {
            return None;
        }

        let phase_position = frac * self.phases as f64;
        let phase0 = phase_position.floor() as usize;
        let phase1 = (phase0 + 1).min(self.phases);
        let alpha = (phase_position - phase0 as f64) as f32;

        let offset0 = phase0 * self.taps;
        let offset1 = phase1 * self.taps;

        let mut left = 0.0f32;
        let mut right = 0.0f32;

        for tap in 0..self.taps {
            let frame = ring.peek(start + tap)?;
            let c0 = self.table[offset0 + tap];
            let c1 = self.table[offset1 + tap];
            let coefficient = c0 + (c1 - c0) * alpha;
            left += frame[0] * coefficient;
            right += frame[1] * coefficient;
        }

        Some([left, right])
    }
}

fn sinc_pi(x: f64) -> f64 {
    if x.abs() < 1.0e-12 {
        1.0
    } else {
        let pix = std::f64::consts::PI * x;
        pix.sin() / pix
    }
}

fn kaiser_window(tap: usize, taps: usize, beta: f64) -> f64 {
    let n = tap as f64;
    let m = (taps - 1) as f64;
    let ratio = 2.0 * n / m - 1.0;
    let shape = (1.0 - ratio * ratio).max(0.0).sqrt();
    bessel_i0(beta * shape) / bessel_i0(beta)
}

fn bessel_i0(x: f64) -> f64 {
    let y = x * x / 4.0;
    let mut sum = 1.0;
    let mut term = 1.0;

    for k in 1..=32 {
        term *= y / ((k * k) as f64);
        sum += term;
        if term.abs() < sum.abs() * 1.0e-15 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fractional_phase_has_unity_dc_gain() {
        let kernel = PolyphaseSinc::reference();
        for phase in 0..=kernel.phases {
            let start = phase * kernel.taps;
            let sum: f32 = kernel.table[start..start + kernel.taps].iter().sum();
            assert!((sum - 1.0).abs() < 2.0e-5);
        }
    }

    #[test]
    fn constant_stereo_signal_remains_constant() {
        let ring = SourcePcmRing::new(256).unwrap();
        let data = vec![[0.25f32, -0.5f32]; 128];
        ring.append(&data);
        let kernel = PolyphaseSinc::reference();

        for frac in [0.0, 0.125, 0.5, 0.875] {
            let frame = kernel
                .sample(&ring, kernel.left_taps() as f64 + 20.0 + frac)
                .unwrap();
            assert!((frame[0] - 0.25).abs() < 1.0e-4);
            assert!((frame[1] + 0.5).abs() < 1.0e-4);
        }
    }
}
