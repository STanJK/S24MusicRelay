//! FIFO-occupancy clock servo.
//!
//! Positive fill error means the source clock is winning and the ASRC must
//! consume source frames faster, so the returned step rises above 1.0.
//! Negative error produces the opposite correction.

#[derive(Clone, Copy, Debug)]
pub struct ClockServoConfig {
    pub target_fill_frames: f64,
    pub kp_ppm_per_frame: f64,
    pub ki_ppm_per_frame_second: f64,
    pub max_abs_ppm: f64,
}

impl Default for ClockServoConfig {
    fn default() -> Self {
        Self {
            target_fill_frames: 1_920.0,
            kp_ppm_per_frame: 0.5,
            ki_ppm_per_frame_second: 0.08,
            max_abs_ppm: 2_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClockServoOutput {
    pub step: f64,
    pub correction_ppm: f64,
    pub fill_error_frames: f64,
}

#[derive(Debug)]
pub struct ClockServo {
    config: ClockServoConfig,
    integral_frame_seconds: f64,
}

impl ClockServo {
    pub fn new(config: ClockServoConfig) -> Result<Self, &'static str> {
        if config.target_fill_frames <= 0.0
            || config.kp_ppm_per_frame < 0.0
            || config.ki_ppm_per_frame_second < 0.0
            || config.max_abs_ppm <= 0.0
        {
            return Err("invalid clock-servo configuration");
        }

        Ok(Self {
            config,
            integral_frame_seconds: 0.0,
        })
    }

    pub fn reference() -> Self {
        Self::new(ClockServoConfig::default())
            .expect("reference servo parameters are valid")
    }

    pub fn target_fill_frames(&self) -> usize {
        self.config.target_fill_frames.round() as usize
    }

    pub fn update(
        &mut self,
        observed_fill_frames: usize,
        output_frames: usize,
        output_sample_rate_hz: f64,
    ) -> ClockServoOutput {
        let error =
            observed_fill_frames as f64 - self.config.target_fill_frames;
        let dt = output_frames as f64 / output_sample_rate_hz.max(1.0);

        self.integral_frame_seconds += error * dt;

        let integral_limit = if self.config.ki_ppm_per_frame_second > 0.0 {
            self.config.max_abs_ppm
                / self.config.ki_ppm_per_frame_second
        } else {
            0.0
        };

        if integral_limit > 0.0 {
            self.integral_frame_seconds = self
                .integral_frame_seconds
                .clamp(-integral_limit, integral_limit);
        }

        let ppm = (
            self.config.kp_ppm_per_frame * error
                + self.config.ki_ppm_per_frame_second
                    * self.integral_frame_seconds
        )
            .clamp(
                -self.config.max_abs_ppm,
                self.config.max_abs_ppm,
            );

        ClockServoOutput {
            step: 1.0 + ppm * 1.0e-6,
            correction_ppm: ppm,
            fill_error_frames: error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_sign_matches_fifo_error() {
        let mut high = ClockServo::reference();
        let high_out = high.update(2_400, 480, 48_000.0);
        assert!(high_out.step > 1.0);

        let mut low = ClockServo::reference();
        let low_out = low.update(1_200, 480, 48_000.0);
        assert!(low_out.step < 1.0);
    }

    #[test]
    fn correction_is_bounded() {
        let mut servo = ClockServo::reference();
        let out = servo.update(100_000, 480, 48_000.0);
        assert!(out.correction_ppm <= 2_000.0);
    }
}
