//! One Windows relay generation.
//!
//! Runtime wires transport -> media -> platform, but does not absorb ownership
//! of their internal state machines.

#[derive(Debug, Default)]
pub struct RelayWorker;
