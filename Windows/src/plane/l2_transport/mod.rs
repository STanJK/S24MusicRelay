//! L2 transport plane.
//!
//! Owns UDP datagrams and converts unreliable packet arrival into an ordered
//! source-frame stream. It must not own ASRC, DSP, or WASAPI timing.

pub mod protocol;
pub mod receiver;
pub mod reorder;
