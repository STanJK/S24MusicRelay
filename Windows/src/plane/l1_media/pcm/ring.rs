//! Bounded SPSC stereo Float32 source FIFO.
//!
//! Unlike a conventional destructive block ring, the ASRC consumer can peek
//! ahead without retiring samples. Retirement is explicit so the fractional FIR
//! keeps its required history window while the producer can continue appending.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Debug)]
struct StereoSlot {
    left: AtomicU32,
    right: AtomicU32,
}

impl StereoSlot {
    fn zero() -> Self {
        Self {
            left: AtomicU32::new(0.0f32.to_bits()),
            right: AtomicU32::new(0.0f32.to_bits()),
        }
    }

    fn store(&self, frame: [f32; 2]) {
        self.left.store(frame[0].to_bits(), Ordering::Relaxed);
        self.right.store(frame[1].to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> [f32; 2] {
        [
            f32::from_bits(self.left.load(Ordering::Relaxed)),
            f32::from_bits(self.right.load(Ordering::Relaxed)),
        ]
    }
}

#[derive(Debug)]
pub struct SourcePcmRing {
    slots: Box<[StereoSlot]>,
    capacity_frames: usize,
    write_frames: AtomicU64,
    read_frames: AtomicU64,
}

impl SourcePcmRing {
    pub fn new(capacity_frames: usize) -> Result<Self, &'static str> {
        if capacity_frames == 0 {
            return Err("PCM ring capacity must be nonzero");
        }

        Ok(Self {
            slots: (0..capacity_frames)
                .map(|_| StereoSlot::zero())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            capacity_frames,
            write_frames: AtomicU64::new(0),
            read_frames: AtomicU64::new(0),
        })
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    pub fn available_frames(&self) -> usize {
        let write = self.write_frames.load(Ordering::Acquire);
        let read = self.read_frames.load(Ordering::Acquire);
        usize::try_from(write.saturating_sub(read))
            .unwrap_or(usize::MAX)
            .min(self.capacity_frames)
    }

    pub fn append(&self, source: &[[f32; 2]]) -> usize {
        if source.is_empty() {
            return 0;
        }

        let write = self.write_frames.load(Ordering::Relaxed);
        let read = self.read_frames.load(Ordering::Acquire);
        let used = usize::try_from(write.saturating_sub(read))
            .unwrap_or(usize::MAX)
            .min(self.capacity_frames);
        let free = self.capacity_frames.saturating_sub(used);
        let frames_to_write = source.len().min(free);

        for (offset, frame) in source.iter().take(frames_to_write).enumerate() {
            let slot = ((write + offset as u64) % self.capacity_frames as u64) as usize;
            self.slots[slot].store(*frame);
        }

        self.write_frames
            .store(write + frames_to_write as u64, Ordering::Release);
        frames_to_write
    }

    pub fn peek(&self, relative_frame: usize) -> Option<[f32; 2]> {
        let read = self.read_frames.load(Ordering::Acquire);
        let write = self.write_frames.load(Ordering::Acquire);
        let available = usize::try_from(write.saturating_sub(read))
            .unwrap_or(usize::MAX)
            .min(self.capacity_frames);

        if relative_frame >= available {
            return None;
        }

        let slot =
            ((read + relative_frame as u64) % self.capacity_frames as u64) as usize;
        Some(self.slots[slot].load())
    }

    pub fn retire(&self, requested_frames: usize) -> usize {
        if requested_frames == 0 {
            return 0;
        }

        let read = self.read_frames.load(Ordering::Relaxed);
        let write = self.write_frames.load(Ordering::Acquire);
        let available = usize::try_from(write.saturating_sub(read))
            .unwrap_or(usize::MAX)
            .min(self.capacity_frames);
        let retired = requested_frames.min(available);

        if retired > 0 {
            self.read_frames
                .store(read + retired as u64, Ordering::Release);
        }
        retired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_does_not_retire_and_wrap_preserves_order() {
        let ring = SourcePcmRing::new(4).unwrap();
        assert_eq!(ring.append(&[[1.0, -1.0], [2.0, -2.0], [3.0, -3.0]]), 3);
        assert_eq!(ring.peek(0), Some([1.0, -1.0]));
        assert_eq!(ring.peek(2), Some([3.0, -3.0]));
        assert_eq!(ring.available_frames(), 3);

        assert_eq!(ring.retire(2), 2);
        assert_eq!(ring.append(&[[4.0, -4.0], [5.0, -5.0]]), 2);
        assert_eq!(ring.peek(0), Some([3.0, -3.0]));
        assert_eq!(ring.peek(1), Some([4.0, -4.0]));
        assert_eq!(ring.peek(2), Some([5.0, -5.0]));
    }

    #[test]
    fn append_is_bounded() {
        let ring = SourcePcmRing::new(2).unwrap();
        assert_eq!(ring.append(&[[1.0, 1.0], [2.0, 2.0], [3.0, 3.0]]), 2);
        assert_eq!(ring.available_frames(), 2);
    }
}
