//! Low-volume runtime telemetry contracts.
//!
//! Planned counters include packet sequence/loss/reorder, source timeline,
//! PCM fill, ASRC ratio/ppm, underrun/overrun, and render progress.

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeTelemetrySnapshot {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub packets_reordered: u64,
    pub source_fill_frames: u64,
    pub render_underflow_frames: u64,
}
