//! Bounded single-producer/single-consumer stereo Float32 source FIFO.
//!
//! Contract target:
//! - one producer after transport normalization;
//! - one ASRC consumer;
//! - append contiguous frames;
//! - inspect a fractional-resampler window without retiring it;
//! - explicitly retire consumed integer source frames;
//! - no network semantics.

#[derive(Debug, Default)]
pub struct SourcePcmRing;
