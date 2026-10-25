//! Consumer-owned ASRC runtime.
//!
//! Owns fractional source phase and coordinates SourcePcmRing + ClockServo +
//! resampler kernel. The Windows render clock is the output pacing authority.

#[derive(Debug, Default)]
pub struct AsrcRuntime;
