//! Packet reorder/loss normalization.
//!
//! Output from this boundary must represent one monotonic source-frame
//! timeline before PCM enters L1 Media.

#[derive(Debug, Default)]
pub struct ReorderBoundary;
