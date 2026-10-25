//! Process-loopback source ownership for S24MusicRelay.
//!
//! The Win32 capture callback is kept intentionally thin: copy one packet,
//! assign source-frame identity, and try-send it into a bounded queue.
//! LC3 encoding and UDP transmission live on the consumer side.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    time::Duration,
};

use crate::platform::windows::process_loopback::{
    ProcessLoopbackCapture, ProcessLoopbackCaptureStats, ProcessLoopbackError,
};

#[derive(Clone, Debug)]
pub struct ProcessLoopbackChunk {
    pub first_frame_id: u64,
    pub frames: Vec<[f32; 2]>,
    pub discontinuity: bool,
    pub device_position: u64,
    pub qpc_position: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessLoopbackSourceStats {
    pub capture_packets: u64,
    pub captured_frames: u64,
    pub queue_dropped_packets: u64,
    pub queue_dropped_frames: u64,
    pub discontinuities: u64,
    pub last_device_position: u64,
    pub last_qpc_position: u64,
}

#[derive(Default)]
struct SourceCounters {
    capture_packets: AtomicU64,
    captured_frames: AtomicU64,
    queue_dropped_packets: AtomicU64,
    queue_dropped_frames: AtomicU64,
    discontinuities: AtomicU64,
    last_device_position: AtomicU64,
    last_qpc_position: AtomicU64,
    next_frame_id: AtomicU64,
}

impl SourceCounters {
    fn snapshot(&self) -> ProcessLoopbackSourceStats {
        ProcessLoopbackSourceStats {
            capture_packets: self.capture_packets.load(Ordering::Relaxed),
            captured_frames: self.captured_frames.load(Ordering::Relaxed),
            queue_dropped_packets: self
                .queue_dropped_packets
                .load(Ordering::Relaxed),
            queue_dropped_frames: self
                .queue_dropped_frames
                .load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            last_device_position: self
                .last_device_position
                .load(Ordering::Relaxed),
            last_qpc_position: self
                .last_qpc_position
                .load(Ordering::Relaxed),
        }
    }
}

pub struct ProcessLoopbackSource {
    capture: Option<ProcessLoopbackCapture>,
    receiver: Receiver<ProcessLoopbackChunk>,
    counters: Arc<SourceCounters>,
}

impl ProcessLoopbackSource {
    pub fn start_excluding_current_process_tree(
        queue_capacity_packets: usize,
    ) -> Result<Self, ProcessLoopbackError> {
        let capacity = queue_capacity_packets.max(1);
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let counters = Arc::new(SourceCounters::default());
        let callback_counters = Arc::clone(&counters);

        let capture = ProcessLoopbackCapture::start_excluding_process_tree(
            std::process::id(),
            move |packet| {
                let frame_count = packet.frames.len();
                let first_frame_id = callback_counters
                    .next_frame_id
                    .fetch_add(frame_count as u64, Ordering::Relaxed);

                callback_counters
                    .capture_packets
                    .fetch_add(1, Ordering::Relaxed);
                callback_counters
                    .captured_frames
                    .fetch_add(frame_count as u64, Ordering::Relaxed);
                if packet.discontinuity {
                    callback_counters
                        .discontinuities
                        .fetch_add(1, Ordering::Relaxed);
                }
                callback_counters.last_device_position.store(
                    packet.device_position,
                    Ordering::Relaxed,
                );
                callback_counters.last_qpc_position.store(
                    packet.qpc_position,
                    Ordering::Relaxed,
                );

                let frames = if packet.silent {
                    vec![[0.0f32, 0.0f32]; frame_count]
                } else {
                    packet.frames.to_vec()
                };

                let chunk = ProcessLoopbackChunk {
                    first_frame_id,
                    frames,
                    discontinuity: packet.discontinuity,
                    device_position: packet.device_position,
                    qpc_position: packet.qpc_position,
                };

                match sender.try_send(chunk) {
                    Ok(()) => {}
                    Err(TrySendError::Full(chunk)) => {
                        callback_counters
                            .queue_dropped_packets
                            .fetch_add(1, Ordering::Relaxed);
                        callback_counters
                            .queue_dropped_frames
                            .fetch_add(
                                chunk.frames.len() as u64,
                                Ordering::Relaxed,
                            );
                    }
                    Err(TrySendError::Disconnected(_)) => {}
                }
            },
        )?;

        Ok(Self {
            capture: Some(capture),
            receiver,
            counters,
        })
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ProcessLoopbackChunk, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub fn stats(&self) -> ProcessLoopbackSourceStats {
        self.counters.snapshot()
    }

    pub fn shutdown(
        mut self,
    ) -> Result<
        (ProcessLoopbackSourceStats, ProcessLoopbackCaptureStats),
        ProcessLoopbackError,
    > {
        let capture = self
            .capture
            .take()
            .ok_or(ProcessLoopbackError::StartupChannelClosed)?;
        let platform = capture.shutdown()?;
        Ok((self.counters.snapshot(), platform))
    }
}

impl Drop for ProcessLoopbackSource {
    fn drop(&mut self) {
        self.capture.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_stats_default_to_zero() {
        assert_eq!(
            SourceCounters::default().snapshot(),
            ProcessLoopbackSourceStats::default()
        );
    }
}
