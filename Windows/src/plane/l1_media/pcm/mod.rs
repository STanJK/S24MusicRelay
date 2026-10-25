//! PCM storage boundary.
//!
//! The ASRC consumer requires peek + retire semantics rather than only
//! destructive fixed-block reads.

pub mod ring;
