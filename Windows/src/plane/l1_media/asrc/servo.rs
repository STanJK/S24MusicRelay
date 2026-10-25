//! Clock servo driven by source-FIFO occupancy.
//!
//! It estimates long-term source/render clock mismatch and produces a bounded
//! ASRC step near 1.0. Network jitter policy does not live here.

#[derive(Clone, Copy, Debug)]
pub struct ClockServoOutput {
    pub step: f64,
    pub estimated_ppm: f64,
    pub fill_error_frames: f64,
}

impl Default for ClockServoOutput {
    fn default() -> Self {
        Self {
            step: 1.0,
            estimated_ppm: 0.0,
            fill_error_frames: 0.0,
        }
    }
}
