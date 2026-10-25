//! Two-timescale FIFO-occupancy clock servo.
//!
//! Raw producer fill is intentionally NOT fed straight into the controller.
//! Packet/block arrival granularity and scheduler jitter create a high-rate
//! sawtooth that is unrelated to oscillator drift. The servo therefore:
//!
//! 1. low-pass filters post-render residual FIFO occupancy;
//! 2. uses a bounded proportional term for small/fast cushion correction;
//! 3. learns long-term source/render mismatch in a slow integral term;
//! 4. keeps acquisition separate so startup transients do not poison drift.
//!
//! Positive residual error means the source clock is winning and the next
//! ASRC step must rise above 1.0. Negative error means the opposite.

#[derive(Clone, Copy, Debug)]
pub struct ClockServoConfig {
    pub target_fill_frames: f64,
    pub occupancy_filter_tau_seconds: f64,
    pub acquisition_seconds: f64,

    pub buffer_kp_ppm_per_frame: f64,
    pub max_abs_buffer_ppm: f64,

    pub drift_ki_ppm_per_frame_second: f64,
    pub max_abs_drift_ppm: f64,

    pub max_abs_total_ppm: f64,
}

impl Default for ClockServoConfig {
    fn default() -> Self {
        Self {
            target_fill_frames: 1_920.0,
            occupancy_filter_tau_seconds: 0.5,
            acquisition_seconds: 1.0,

            buffer_kp_ppm_per_frame: 5.0,
            max_abs_buffer_ppm: 750.0,

            drift_ki_ppm_per_frame_second: 1.0,
            max_abs_drift_ppm: 1_500.0,

            max_abs_total_ppm: 2_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClockServoOutput {
    pub step: f64,

    pub correction_ppm: f64,
    pub drift_estimate_ppm: f64,
    pub buffer_correction_ppm: f64,

    pub raw_fill_frames: usize,
    pub filtered_fill_frames: f64,
    pub fill_error_frames: f64,

    pub acquiring: bool,
}

#[derive(Debug)]
pub struct ClockServo {
    config: ClockServoConfig,

    filtered_fill_frames: f64,
    elapsed_seconds: f64,
    drift_estimate_ppm: f64,
}

impl ClockServo {
    pub fn new(config: ClockServoConfig) -> Result<Self, &'static str> {
        if config.target_fill_frames <= 0.0
            || config.occupancy_filter_tau_seconds <= 0.0
            || config.acquisition_seconds < 0.0
            || config.buffer_kp_ppm_per_frame < 0.0
            || config.max_abs_buffer_ppm <= 0.0
            || config.drift_ki_ppm_per_frame_second < 0.0
            || config.max_abs_drift_ppm <= 0.0
            || config.max_abs_total_ppm <= 0.0
        {
            return Err("invalid clock-servo configuration");
        }

        Ok(Self {
            filtered_fill_frames: config.target_fill_frames,
            elapsed_seconds: 0.0,
            drift_estimate_ppm: 0.0,
            config,
        })
    }

    pub fn reference() -> Self {
        Self::new(ClockServoConfig::default())
            .expect("reference servo parameters are valid")
    }

    pub fn target_fill_frames(&self) -> usize {
        self.config.target_fill_frames.round() as usize
    }

    pub fn update_after_render(
        &mut self,
        residual_fill_frames: usize,
        rendered_output_frames: usize,
        output_sample_rate_hz: f64,
    ) -> ClockServoOutput {
        let dt =
            rendered_output_frames as f64 / output_sample_rate_hz.max(1.0);

        let alpha = if dt > 0.0 {
            1.0 - (-dt / self.config.occupancy_filter_tau_seconds).exp()
        } else {
            0.0
        };

        self.filtered_fill_frames +=
            alpha * (residual_fill_frames as f64 - self.filtered_fill_frames);

        self.elapsed_seconds += dt;

        let error =
            self.filtered_fill_frames - self.config.target_fill_frames;

        let buffer_correction_ppm = (
            self.config.buffer_kp_ppm_per_frame * error
        )
            .clamp(
                -self.config.max_abs_buffer_ppm,
                self.config.max_abs_buffer_ppm,
            );

        let acquiring =
            self.elapsed_seconds < self.config.acquisition_seconds;

        if !acquiring && dt > 0.0 {
            let candidate = (
                self.drift_estimate_ppm
                    + self.config.drift_ki_ppm_per_frame_second
                        * error
                        * dt
            )
                .clamp(
                    -self.config.max_abs_drift_ppm,
                    self.config.max_abs_drift_ppm,
                );

            // Basic anti-windup: if the proposed integral update would push the
            // total correction farther into saturation, keep the old drift
            // estimate until the buffer term moves back toward the linear
            // control region.
            let candidate_total =
                candidate + buffer_correction_ppm;
            let would_worsen_saturation =
                candidate_total.abs() > self.config.max_abs_total_ppm
                    && candidate_total.signum() == error.signum();

            if !would_worsen_saturation {
                self.drift_estimate_ppm = candidate;
            }
        }

        let correction_ppm = (
            self.drift_estimate_ppm + buffer_correction_ppm
        )
            .clamp(
                -self.config.max_abs_total_ppm,
                self.config.max_abs_total_ppm,
            );

        ClockServoOutput {
            step: 1.0 + correction_ppm * 1.0e-6,

            correction_ppm,
            drift_estimate_ppm: self.drift_estimate_ppm,
            buffer_correction_ppm,

            raw_fill_frames: residual_fill_frames,
            filtered_fill_frames: self.filtered_fill_frames,
            fill_error_frames: error,

            acquiring,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_packet_sawtooth_is_heavily_filtered() {
        let mut servo = ClockServo::reference();

        let out =
            servo.update_after_render(2_160, 480, 48_000.0);

        assert_eq!(out.raw_fill_frames, 2_160);
        assert!(out.filtered_fill_frames > 1_920.0);
        assert!(out.filtered_fill_frames < 1_930.0);
        assert!(out.buffer_correction_ppm < 50.0);
    }

    #[test]
    fn acquisition_does_not_poison_drift_estimate() {
        let mut servo = ClockServo::reference();

        for _ in 0..50 {
            let out =
                servo.update_after_render(1_600, 480, 48_000.0);
            assert!(out.acquiring);
            assert_eq!(out.drift_estimate_ppm, 0.0);
        }
    }

    #[test]
    fn correction_sign_matches_filtered_fifo_error() {
        let mut high = ClockServo::reference();
        let mut high_out = ClockServoOutput::default();
        for _ in 0..200 {
            high_out =
                high.update_after_render(2_400, 480, 48_000.0);
        }
        assert!(high_out.step > 1.0);

        let mut low = ClockServo::reference();
        let mut low_out = ClockServoOutput::default();
        for _ in 0..200 {
            low_out =
                low.update_after_render(1_200, 480, 48_000.0);
        }
        assert!(low_out.step < 1.0);
    }

    #[test]
    fn closed_loop_learns_plus_500_ppm_and_recenters_fifo() {
        let (fill, out) = simulate_constant_clock_error(500.0, 60.0);

        assert!((fill - 1_920.0).abs() < 10.0);
        assert!((out.drift_estimate_ppm - 500.0).abs() < 25.0);
        assert!((out.correction_ppm - 500.0).abs() < 25.0);
    }

    #[test]
    fn closed_loop_learns_minus_500_ppm_and_recenters_fifo() {
        let (fill, out) = simulate_constant_clock_error(-500.0, 60.0);

        assert!((fill - 1_920.0).abs() < 10.0);
        assert!((out.drift_estimate_ppm + 500.0).abs() < 25.0);
        assert!((out.correction_ppm + 500.0).abs() < 25.0);
    }

    fn simulate_constant_clock_error(
        source_ppm: f64,
        duration_seconds: f64,
    ) -> (f64, ClockServoOutput) {
        const BLOCK_FRAMES: usize = 480;
        const SAMPLE_RATE_HZ: f64 = 48_000.0;

        let mut servo = ClockServo::reference();
        let mut fill = servo.target_fill_frames() as f64;
        let mut step = 1.0;
        let mut out = ClockServoOutput::default();

        let iterations =
            (duration_seconds * SAMPLE_RATE_HZ / BLOCK_FRAMES as f64) as usize;

        for _ in 0..iterations {
            let produced =
                BLOCK_FRAMES as f64 * (1.0 + source_ppm * 1.0e-6);
            let consumed =
                BLOCK_FRAMES as f64 * step;

            fill += produced - consumed;

            out = servo.update_after_render(
                fill.round().max(0.0) as usize,
                BLOCK_FRAMES,
                SAMPLE_RATE_HZ,
            );
            step = out.step;
        }

        (fill, out)
    }
}
