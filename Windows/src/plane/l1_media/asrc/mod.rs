//! Authoritative asynchronous sample-rate conversion boundary.
//!
//! Nominal format remains 48 kHz -> 48 kHz. ASRC exists to cross independent
//! S24 and Windows render clocks.

pub mod kernel;
pub mod runtime;
pub mod servo;
