//! Two-timescale FIFO-occupancy clock servo.
//!
//! Raw producer fill is low-pass filtered before control. Startup acquisition
//! and steady-state drift learning are separate states:
//!
//! - ACQUIRE: bounded proportional cushion control only. No drift learning.
//! - TRACK: slow integral drift estimator + small proportional buffer trim.
//!
//! TRACK is entered only after filtered FIFO error remains inside a bounded
//! acquisition window for a continuous dwell interval. This prevents startup
//! FIFO transients from being mislearned as oscillator mismatch.

#[derive(Clone, Copy, Debug)]
pub struct ClockServoConfig {
    pub target_fill_frames: f64,
    pub acquisition_filter_tau_seconds: f64,
    pub tracking_filter_tau_seconds: f64,

    pub acquisition_error_frames: f64,
    pub acquisition_max_slope_frames_per_second: f64,
    pub acquisition_dwell_seconds: f64,
    pub acquisition_kp_ppm_per_frame: f64,
    pub max_abs_acquisition_ppm: f64,

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

            // Acquisition is intentionally much faster than tracking so startup
            // cushion error is removed quickly without teaching the long-term
            // drift estimator anything about that transient.
            acquisition_filter_tau_seconds: 0.5,
            tracking_filter_tau_seconds: 5.0,

            acquisition_error_frames: 96.0,
            acquisition_max_slope_frames_per_second: 20.0,
            acquisition_dwell_seconds: 1.0,
            acquisition_kp_ppm_per_frame: 8.0,
            max_abs_acquisition_ppm: 4_000.0,

            // Once tracking is active, packet-phase jitter should only make
            // small, slow buffer corrections.
            buffer_kp_ppm_per_frame: 2.0,
            max_abs_buffer_ppm: 500.0,

            drift_ki_ppm_per_frame_second: 0.1,
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
    pub filtered_fill_slope_frames_per_second: f64,

    pub acquiring: bool,
    pub acquisition_stable_seconds: f64,
}

#[derive(Debug)]
pub struct ClockServo {
    config: ClockServoConfig,

    filtered_fill_frames: f64,
    drift_estimate_ppm: f64,

    tracking: bool,
    acquisition_stable_seconds: f64,
}

impl ClockServo {
    pub fn new(config: ClockServoConfig) -> Result<Self, &'static str> {
        if config.target_fill_frames <= 0.0
            || config.acquisition_filter_tau_seconds <= 0.0
            || config.tracking_filter_tau_seconds <= 0.0
            || config.acquisition_error_frames <= 0.0
            || config.acquisition_max_slope_frames_per_second <= 0.0
            || config.acquisition_dwell_seconds <= 0.0
            || config.acquisition_kp_ppm_per_frame < 0.0
            || config.max_abs_acquisition_ppm <= 0.0
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
            drift_estimate_ppm: 0.0,
            tracking: false,
            acquisition_stable_seconds: 0.0,
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

        let filter_tau_seconds = if self.tracking {
            self.config.tracking_filter_tau_seconds
        } else {
            self.config.acquisition_filter_tau_seconds
        };

        let alpha = if dt > 0.0 {
            1.0 - (-dt / filter_tau_seconds).exp()
        } else {
            0.0
        };

        let previous_filtered_fill = self.filtered_fill_frames;
        self.filtered_fill_frames +=
            alpha * (residual_fill_frames as f64 - self.filtered_fill_frames);

        let filtered_fill_slope_frames_per_second =
            if dt > 0.0 {
                (self.filtered_fill_frames - previous_filtered_fill) / dt
            } else {
                0.0
            };

        let error =
            self.filtered_fill_frames - self.config.target_fill_frames;

        let was_tracking = self.tracking;

        if !self.tracking {
            let position_stable =
                error.abs() <= self.config.acquisition_error_frames;
            let motion_stable =
                filtered_fill_slope_frames_per_second.abs()
                    <= self.config.acquisition_max_slope_frames_per_second;

            if position_stable && motion_stable {
                self.acquisition_stable_seconds += dt;
                if self.acquisition_stable_seconds
                    >= self.config.acquisition_dwell_seconds
                {
                    self.tracking = true;
                }
            } else {
                self.acquisition_stable_seconds = 0.0;
            }
        }

        let buffer_correction_ppm = if self.tracking {
            (self.config.buffer_kp_ppm_per_frame * error).clamp(
                -self.config.max_abs_buffer_ppm,
                self.config.max_abs_buffer_ppm,
            )
        } else {
            (self.config.acquisition_kp_ppm_per_frame * error).clamp(
                -self.config.max_abs_acquisition_ppm,
                self.config.max_abs_acquisition_ppm,
            )
        };

        if was_tracking && dt > 0.0 {
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

            let candidate_total =
                candidate + buffer_correction_ppm;
            let would_worsen_saturation =
                candidate_total.abs() > self.config.max_abs_total_ppm
                    && candidate_total.signum() == error.signum();

            if !would_worsen_saturation {
                self.drift_estimate_ppm = candidate;
            }
        }

        let correction_ppm = if self.tracking {
            (self.drift_estimate_ppm + buffer_correction_ppm).clamp(
                -self.config.max_abs_total_ppm,
                self.config.max_abs_total_ppm,
            )
        } else {
            buffer_correction_ppm
        };

        ClockServoOutput {
            step: 1.0 + correction_ppm * 1.0e-6,

            correction_ppm,
            drift_estimate_ppm: self.drift_estimate_ppm,
            buffer_correction_ppm,

            raw_fill_frames: residual_fill_frames,
            filtered_fill_frames: self.filtered_fill_frames,
            fill_error_frames: error,
            filtered_fill_slope_frames_per_second,

            acquiring: !self.tracking,
            acquisition_stable_seconds: self.acquisition_stable_seconds,
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
        assert!(out.buffer_correction_ppm < 100.0);
    }

    #[test]
    fn large_startup_error_cannot_enter_tracking_or_poison_drift() {
        let mut servo = ClockServo::reference();

        for _ in 0..500 {
            let out =
                servo.update_after_render(1_600, 480, 48_000.0);
            assert!(out.acquiring);
            assert_eq!(out.drift_estimate_ppm, 0.0);

            if out.fill_error_frames.abs() <= 96.0 {
                assert!(
                    out.filtered_fill_slope_frames_per_second.abs() > 20.0,
                    "moving startup transient must not satisfy acquisition dwell"
                );
            }
        }
    }

    #[test]
    fn acquisition_requires_continuous_dwell_inside_window() {
        let mut servo = ClockServo::reference();

        // 480 frames at 48 kHz is exactly 10 ms per update.
        // A 1.0 s acquisition dwell therefore requires 100 complete
        // in-window updates before TRACK may become active.
        for _ in 0..99 {
            let out =
                servo.update_after_render(1_920, 480, 48_000.0);
            assert!(out.acquiring);
        }

        let out =
            servo.update_after_render(1_920, 480, 48_000.0);
        assert!(!out.acquiring);
    }

    #[test]
    fn correction_sign_matches_filtered_fifo_error() {
        let mut high = ClockServo::reference();
        let mut high_out = ClockServoOutput::default();
        for _ in 0..400 {
            high_out =
                high.update_after_render(2_400, 480, 48_000.0);
        }
        assert!(high_out.step > 1.0);

        let mut low = ClockServo::reference();
        let mut low_out = ClockServoOutput::default();
        for _ in 0..400 {
            low_out =
                low.update_after_render(1_200, 480, 48_000.0);
        }
        assert!(low_out.step < 1.0);
    }

    #[test]
    fn closed_loop_learns_plus_500_ppm_and_recenters_fifo() {
        let (fill, out) = simulate_constant_clock_error(500.0, 90.0);

        assert!(!out.acquiring);
        assert!((fill - 1_920.0).abs() < 15.0);
        assert!((out.drift_estimate_ppm - 500.0).abs() < 35.0);
        assert!((out.correction_ppm - 500.0).abs() < 35.0);
    }

    #[test]
    fn closed_loop_learns_minus_500_ppm_and_recenters_fifo() {
        let (fill, out) = simulate_constant_clock_error(-500.0, 90.0);

        assert!(!out.acquiring);
        assert!((fill - 1_920.0).abs() < 15.0);
        assert!((out.drift_estimate_ppm + 500.0).abs() < 35.0);
        assert!((out.correction_ppm + 500.0).abs() < 35.0);
    }

    #[test]
    fn packetized_plus_500_ppm_is_stable_on_average() {
        let stats = simulate_packetized_clock_error(500.0, 180.0);
        assert!((stats.mean_total_ppm - 500.0).abs() < 25.0);
        assert!((stats.mean_drift_ppm - 500.0).abs() < 25.0);
        assert!((stats.mean_filtered_fill - 1_920.0).abs() < 25.0);
        // Raw FIFO is packet-phase quantized in 240-frame steps. Do not
        // require it to stay near the filtered target; require only a large
        // safety margin above FIR/underflow territory.
        assert!(stats.minimum_fill > 1_700.0);
    }

    #[test]
    fn packetized_minus_500_ppm_is_stable_on_average() {
        let stats = simulate_packetized_clock_error(-500.0, 180.0);
        assert!((stats.mean_total_ppm + 500.0).abs() < 25.0);
        assert!((stats.mean_drift_ppm + 500.0).abs() < 25.0);
        assert!((stats.mean_filtered_fill - 1_920.0).abs() < 25.0);
        assert!(stats.minimum_fill > 1_700.0);
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct PacketizedStats {
        mean_total_ppm: f64,
        mean_drift_ppm: f64,
        mean_filtered_fill: f64,
        minimum_fill: f64,
    }

    fn simulate_packetized_clock_error(
        source_ppm: f64,
        duration_seconds: f64,
    ) -> PacketizedStats {
        const OUTPUT_FRAMES: usize = 480;
        const SOURCE_PACKET_FRAMES: f64 = 240.0;
        const SAMPLE_RATE_HZ: f64 = 48_000.0;

        let mut servo = ClockServo::reference();
        let mut fill = servo.target_fill_frames() as f64;
        let mut source_phase = 0.0f64;
        let mut step = 1.0;

        let iterations =
            (duration_seconds * SAMPLE_RATE_HZ / OUTPUT_FRAMES as f64) as usize;
        let averaging_start = iterations.saturating_sub(6_000);

        let mut total_sum = 0.0;
        let mut drift_sum = 0.0;
        let mut filtered_sum = 0.0;
        let mut samples = 0usize;
        let mut minimum_fill = f64::MAX;

        for iteration in 0..iterations {
            source_phase += OUTPUT_FRAMES as f64
                * (1.0 + source_ppm * 1.0e-6);

            let packet_count =
                (source_phase / SOURCE_PACKET_FRAMES).floor();
            source_phase -= packet_count * SOURCE_PACKET_FRAMES;
            fill += packet_count * SOURCE_PACKET_FRAMES;

            fill -= OUTPUT_FRAMES as f64 * step;
            minimum_fill = minimum_fill.min(fill);

            let out = servo.update_after_render(
                fill.round().max(0.0) as usize,
                OUTPUT_FRAMES,
                SAMPLE_RATE_HZ,
            );
            step = out.step;

            if iteration >= averaging_start {
                total_sum += out.correction_ppm;
                drift_sum += out.drift_estimate_ppm;
                filtered_sum += out.filtered_fill_frames;
                samples += 1;
            }
        }

        PacketizedStats {
            mean_total_ppm: total_sum / samples as f64,
            mean_drift_ppm: drift_sum / samples as f64,
            mean_filtered_fill: filtered_sum / samples as f64,
            minimum_fill,
        }
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
