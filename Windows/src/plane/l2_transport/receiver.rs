//! UDP socket owner and producer edge into the Media PCM ring.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use socket2::{Domain, Protocol, Socket, Type};

use crate::plane::l1_media::pcm::ring::SourcePcmRing;

use super::{
    protocol,
    reorder::ReorderBoundary,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct UdpReceiverStats {
    pub packets_received: u64,
    pub decode_errors: u64,
    pub reordered_arrivals: u64,
    pub duplicate_or_stale_packets: u64,
    pub lost_packets: u64,
    pub concealed_gap_frames: u64,
    pub skipped_gap_frames: u64,
    pub ring_dropped_frames: u64,
    pub last_sequence: u64,
    pub last_first_frame_id: u64,
}

#[derive(Debug, Default)]
struct Counters {
    packets_received: AtomicU64,
    decode_errors: AtomicU64,
    reordered_arrivals: AtomicU64,
    duplicate_or_stale_packets: AtomicU64,
    lost_packets: AtomicU64,
    concealed_gap_frames: AtomicU64,
    skipped_gap_frames: AtomicU64,
    ring_dropped_frames: AtomicU64,
    last_sequence: AtomicU64,
    last_first_frame_id: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> UdpReceiverStats {
        UdpReceiverStats {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            reordered_arrivals: self
                .reordered_arrivals
                .load(Ordering::Relaxed),
            duplicate_or_stale_packets: self
                .duplicate_or_stale_packets
                .load(Ordering::Relaxed),
            lost_packets: self.lost_packets.load(Ordering::Relaxed),
            concealed_gap_frames: self
                .concealed_gap_frames
                .load(Ordering::Relaxed),
            skipped_gap_frames: self
                .skipped_gap_frames
                .load(Ordering::Relaxed),
            ring_dropped_frames: self
                .ring_dropped_frames
                .load(Ordering::Relaxed),
            last_sequence: self.last_sequence.load(Ordering::Relaxed),
            last_first_frame_id: self
                .last_first_frame_id
                .load(Ordering::Relaxed),
        }
    }
}

pub struct UdpReceiver {
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<()>>,
    pub local_addr: SocketAddr,
    pub recv_buffer_bytes: usize,
}

impl UdpReceiver {
    pub fn start(
        bind: &str,
        ring: Arc<SourcePcmRing>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let bind_addr: SocketAddr = bind.parse()?;
        let domain = if bind_addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let raw = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        raw.set_recv_buffer_size(4 * 1024 * 1024)?;
        raw.bind(&bind_addr.into())?;

        let recv_buffer_bytes = raw.recv_buffer_size()?;
        let socket: UdpSocket = raw.into();
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let local_addr = socket.local_addr()?;

        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());

        let stop_thread = Arc::clone(&stop);
        let counters_thread = Arc::clone(&counters);

        let thread = thread::spawn(move || {
            let mut reorder = ReorderBoundary::default();
            let mut buffer = [0u8; 2048];
            let mut previous_reorder = reorder.stats();

            while !stop_thread.load(Ordering::Acquire) {
                let size = match socket.recv_from(&mut buffer) {
                    Ok((size, _peer)) => size,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                };

                counters_thread
                    .packets_received
                    .fetch_add(1, Ordering::Relaxed);

                let packet = match protocol::decode(&buffer[..size]) {
                    Ok(packet) => packet,
                    Err(_) => {
                        counters_thread
                            .decode_errors
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };

                counters_thread
                    .last_sequence
                    .store(packet.sequence, Ordering::Relaxed);
                counters_thread
                    .last_first_frame_id
                    .store(packet.first_frame_id, Ordering::Relaxed);

                let chunks = reorder.push(packet);
                let current_reorder = reorder.stats();

                counters_thread.reordered_arrivals.fetch_add(
                    current_reorder.reordered_arrivals
                        - previous_reorder.reordered_arrivals,
                    Ordering::Relaxed,
                );
                counters_thread.duplicate_or_stale_packets.fetch_add(
                    current_reorder.duplicate_or_stale_packets
                        - previous_reorder.duplicate_or_stale_packets,
                    Ordering::Relaxed,
                );
                counters_thread.lost_packets.fetch_add(
                    current_reorder.lost_packets
                        - previous_reorder.lost_packets,
                    Ordering::Relaxed,
                );
                counters_thread.concealed_gap_frames.fetch_add(
                    current_reorder.concealed_gap_frames
                        - previous_reorder.concealed_gap_frames,
                    Ordering::Relaxed,
                );
                counters_thread.skipped_gap_frames.fetch_add(
                    current_reorder.skipped_gap_frames
                        - previous_reorder.skipped_gap_frames,
                    Ordering::Relaxed,
                );
                previous_reorder = current_reorder;

                for chunk in chunks {
                    let written = ring.append(&chunk.frames);

                    if written < chunk.frames.len() {
                        counters_thread.ring_dropped_frames.fetch_add(
                            (chunk.frames.len() - written) as u64,
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        });

        Ok(Self {
            stop,
            counters,
            thread: Some(thread),
            local_addr,
            recv_buffer_bytes,
        })
    }

    pub fn stats(&self) -> UdpReceiverStats {
        self.counters.snapshot()
    }

    pub fn stop(mut self) -> UdpReceiverStats {
        self.stop.store(true, Ordering::Release);

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }

        self.stats()
    }
}
