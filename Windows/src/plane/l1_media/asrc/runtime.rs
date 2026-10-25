//! Consumer-owned ASRC runtime.
//!
//! The output clock owns the render request. For each output frame the ASRC
//! advances through the source timeline by `step = source_frames/output_frame`.
//! Old integer source frames are retired only after the FIR history window no
//! longer needs them.

use super::kernel::PolyphaseSinc;
use crate::plane::l1_media::pcm::ring::SourcePcmRing;

#[derive(Clone, Copy, Debug, Default)]
pub struct AsrcRenderResult {
    pub requested_output_frames: usize,
    pub real_output_frames: usize,
    pub underflow_frames: usize,
    pub retired_source_frames: usize,
    pub source_phase_fraction: f64,
}

#[derive(Debug)]
pub struct AsrcRuntime {
    kernel: PolyphaseSinc,
    source_position: f64,
}

impl AsrcRuntime {
    pub fn reference() -> Self {
        let kernel = PolyphaseSinc::reference();
        let source_position = kernel.left_taps() as f64;
        Self {
            kernel,
            source_position,
        }
    }

    pub fn minimum_source_frames(&self) -> usize {
        self.kernel.taps()
    }

    pub fn render(
        &mut self,
        ring: &SourcePcmRing,
        output: &mut [[f32; 2]],
        step: f64,
    ) -> AsrcRenderResult {
        assert!(step.is_finite() && (0.95..=1.05).contains(&step));

        let mut real_output_frames = 0usize;

        for frame in output.iter_mut() {
            match self.kernel.sample(ring, self.source_position) {
                Some(sample) => {
                    *frame = sample;
                    self.source_position += step;
                    real_output_frames += 1;
                }
                None => {
                    *frame = [0.0, 0.0];
                }
            }
        }

        let integer_position = self.source_position.floor() as usize;
        let retire_requested =
            integer_position.saturating_sub(self.kernel.left_taps());
        let retired = ring.retire(retire_requested);
        self.source_position -= retired as f64;

        AsrcRenderResult {
            requested_output_frames: output.len(),
            real_output_frames,
            underflow_frames: output.len() - real_output_frames,
            retired_source_frames: retired,
            source_phase_fraction: self.source_position.fract(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_step_retires_approximately_one_source_frame_per_output() {
        let ring = SourcePcmRing::new(4096).unwrap();
        ring.append(&vec![[0.1, -0.1]; 2048]);

        let mut asrc = AsrcRuntime::reference();
        let mut output = vec![[0.0; 2]; 480];
        let result = asrc.render(&ring, &mut output, 1.0);

        assert_eq!(result.underflow_frames, 0);
        assert_eq!(result.retired_source_frames, 480);
        assert_eq!(ring.available_frames(), 2048 - 480);
    }

    #[test]
    fn faster_source_step_retires_more_than_output_count_over_time() {
        let ring = SourcePcmRing::new(8192).unwrap();
        ring.append(&vec![[0.1, -0.1]; 4096]);

        let mut asrc = AsrcRuntime::reference();
        let mut output = vec![[0.0; 2]; 2000];
        let result = asrc.render(&ring, &mut output, 1.001);

        assert_eq!(result.underflow_frames, 0);
        assert!(result.retired_source_frames > 2000);
    }
}
