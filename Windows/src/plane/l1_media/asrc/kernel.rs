//! Band-limited fractional resampler kernel.
//!
//! The eventual implementation is intentionally independent of buffer-control
//! policy so it can be reused and golden-tested in other projects.

#[derive(Clone, Copy, Debug, Default)]
pub struct AsrcRenderResult {
    pub output_frames: usize,
    pub retired_source_frames: usize,
    pub source_phase_fraction: f64,
}
