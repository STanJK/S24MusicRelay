//! Synthetic independently-paced PCM producer used to validate ASRC.
//!
//! It intentionally runs on its own wall-clock schedule so `clock_ppm` can
//! emulate a source oscillator that is faster or slower than the WASAPI render
//! clock.

use std::{
    f64::consts::TAU,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::plane::l1_media::pcm::ring::SourcePcmRing;

const SAMPLE_RATE_HZ: f64 = 48_000.0;
const BLOCK_FRAMES: usize = 240;

#[derive(Debug)]
pub struct SyntheticProducer {
    stop: Arc<AtomicBool>,
    dropped_frames: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl SyntheticProducer {
    pub fn start(
        ring: Arc<SourcePcmRing>,
        frequency_hz: f64,
        amplitude: f64,
        clock_ppm: f64,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let stop_thread = Arc::clone(&stop);
        let dropped_thread = Arc::clone(&dropped_frames);

        let thread = thread::spawn(move || {
            let effective_rate =
                SAMPLE_RATE_HZ * (1.0 + clock_ppm * 1.0e-6);
            let block_period =
                Duration::from_secs_f64(BLOCK_FRAMES as f64 / effective_rate);

            let mut phase = 0.0f64;
            let phase_step = TAU * frequency_hz / SAMPLE_RATE_HZ;
            let mut deadline = Instant::now();

            while !stop_thread.load(Ordering::Acquire) {
                let mut block = [[0.0f32; 2]; BLOCK_FRAMES];
                for frame in &mut block {
                    let sample = (phase.sin() * amplitude) as f32;
                    *frame = [sample, sample];
                    phase += phase_step;
                    if phase >= TAU {
                        phase -= TAU;
                    }
                }

                let written = ring.append(&block);
                if written < BLOCK_FRAMES {
                    dropped_thread.fetch_add(
                        (BLOCK_FRAMES - written) as u64,
                        Ordering::Relaxed,
                    );
                }

                deadline += block_period;
                let now = Instant::now();
                if deadline > now {
                    thread::sleep(deadline - now);
                } else if now.duration_since(deadline) > block_period * 4 {
                    deadline = now;
                }
            }
        });

        Self {
            stop,
            dropped_frames,
            thread: Some(thread),
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    pub fn stop(mut self) -> u64 {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.dropped_frames()
    }
}

impl Drop for SyntheticProducer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
