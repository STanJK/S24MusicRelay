//! Small bounded packet-reorder/loss normalizer.
//!
//! UDP semantics terminate here. Output is one monotonic PCM source timeline.
//! Missing transport packets or sender-side capture gaps become explicit zeros
//! before Media sees them.

use std::collections::BTreeMap;

use super::protocol::AudioPacket;

const MAX_PENDING_PACKETS: usize = 4;
const MAX_GAP_CONCEAL_FRAMES: usize = 9_600; // 200 ms @ 48 kHz

#[derive(Clone, Debug, Default)]
pub struct ReorderStats {
    pub reordered_arrivals: u64,
    pub duplicate_or_stale_packets: u64,
    pub lost_packets: u64,
    pub concealed_gap_frames: u64,
    pub timeline_resets: u64,
}

#[derive(Clone, Debug)]
pub struct NormalizedChunk {
    pub first_frame_id: u64,
    pub frames: Vec<[f32; 2]>,
}

#[derive(Debug, Default)]
pub struct ReorderBoundary {
    session_id: Option<u64>,
    next_sequence: u64,
    next_frame_id: u64,
    pending: BTreeMap<u64, AudioPacket>,
    stats: ReorderStats,
}

impl ReorderBoundary {
    pub fn stats(&self) -> ReorderStats {
        self.stats.clone()
    }

    pub fn push(&mut self, packet: AudioPacket) -> Vec<NormalizedChunk> {
        let mut output = Vec::new();

        if self.session_id != Some(packet.session_id) {
            if self.session_id.is_some() {
                self.stats.timeline_resets += 1;
            }
            self.session_id = Some(packet.session_id);
            self.next_sequence = packet.sequence;
            self.next_frame_id = packet.first_frame_id;
            self.pending.clear();
        }

        if packet.sequence < self.next_sequence {
            self.stats.duplicate_or_stale_packets += 1;
            return output;
        }

        if packet.sequence > self.next_sequence {
            self.stats.reordered_arrivals += 1;
        }

        if self.pending.insert(packet.sequence, packet).is_some() {
            self.stats.duplicate_or_stale_packets += 1;
        }

        self.drain_ready(&mut output);

        if self.pending.len() >= MAX_PENDING_PACKETS {
            if let Some((first_sequence, first_frame_id)) =
                self.pending
                    .first_key_value()
                    .map(|(&sequence, packet)| (sequence, packet.first_frame_id))
            {
                if first_sequence > self.next_sequence {
                    self.stats.lost_packets +=
                        first_sequence - self.next_sequence;

                    let gap = first_frame_id
                        .saturating_sub(self.next_frame_id)
                        as usize;

                    if gap > 0 && gap <= MAX_GAP_CONCEAL_FRAMES {
                        output.push(NormalizedChunk {
                            first_frame_id: self.next_frame_id,
                            frames: vec![[0.0, 0.0]; gap],
                        });
                        self.stats.concealed_gap_frames += gap as u64;
                    }

                    self.next_sequence = first_sequence;
                    self.next_frame_id = first_frame_id;
                    self.drain_ready(&mut output);
                }
            }
        }

        output
    }

    fn drain_ready(&mut self, output: &mut Vec<NormalizedChunk>) {
        while let Some(mut packet) = self.pending.remove(&self.next_sequence) {
            if packet.first_frame_id > self.next_frame_id {
                let gap =
                    (packet.first_frame_id - self.next_frame_id) as usize;

                if gap <= MAX_GAP_CONCEAL_FRAMES {
                    output.push(NormalizedChunk {
                        first_frame_id: self.next_frame_id,
                        frames: vec![[0.0, 0.0]; gap],
                    });
                    self.stats.concealed_gap_frames += gap as u64;
                }

                self.next_frame_id = packet.first_frame_id;
            } else if packet.first_frame_id < self.next_frame_id {
                let overlap =
                    (self.next_frame_id - packet.first_frame_id) as usize;

                if overlap >= packet.frames.len() {
                    self.stats.duplicate_or_stale_packets += 1;
                    self.next_sequence += 1;
                    continue;
                }

                packet.frames.drain(0..overlap);
                packet.first_frame_id += overlap as u64;
            }

            let first_frame_id = packet.first_frame_id;
            let frame_count = packet.frames.len();

            output.push(NormalizedChunk {
                first_frame_id,
                frames: packet.frames,
            });

            self.next_sequence += 1;
            self.next_frame_id =
                first_frame_id + frame_count as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(seq: u64, first: u64, value: f32) -> AudioPacket {
        AudioPacket {
            session_id: 1,
            sequence: seq,
            first_frame_id: first,
            frames: vec![[value, value]; 240],
        }
    }

    #[test]
    fn reorders_short_out_of_order_run() {
        let mut r = ReorderBoundary::default();

        assert_eq!(r.push(packet(0, 0, 0.0)).len(), 1);
        assert!(r.push(packet(2, 480, 2.0)).is_empty());

        let out = r.push(packet(1, 240, 1.0));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].first_frame_id, 240);
        assert_eq!(out[1].first_frame_id, 480);
    }

    #[test]
    fn source_frame_gap_becomes_silence() {
        let mut r = ReorderBoundary::default();
        let _ = r.push(packet(0, 0, 0.0));

        let out = r.push(packet(1, 480, 1.0));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].first_frame_id, 240);
        assert_eq!(out[0].frames.len(), 240);
        assert!(out[0].frames.iter().all(|f| *f == [0.0, 0.0]));
        assert_eq!(out[1].first_frame_id, 480);
    }
}
