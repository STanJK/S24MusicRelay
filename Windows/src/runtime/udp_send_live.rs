//! Live Windows Process Loopback -> LC3 -> UDP -> Android speaker.

use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct UdpSendLiveConfig {
    pub target: String,
    pub seconds: u64,
    pub sacrificial_substring: String,
    pub gain: f32,
}

#[derive(Default)]
struct FrameAssembler {
    frames: VecDeque<[f32; 2]>,
    buffer_start_frame_id: Option<u64>,
    expected_input_frame_id: Option<u64>,
    resets: u64,
}

impl FrameAssembler {
    fn push(
        &mut self,
        first_frame_id: u64,
        input: &[[f32; 2]],
        discontinuity: bool,
    ) {
        let expected = self.expected_input_frame_id;
        let timeline_break = expected
            .map(|value| value != first_frame_id)
            .unwrap_or(false);

        if discontinuity || timeline_break {
            self.frames.clear();
            self.buffer_start_frame_id = Some(first_frame_id);
            self.resets = self.resets.saturating_add(1);
        } else if self.buffer_start_frame_id.is_none() {
            self.buffer_start_frame_id = Some(first_frame_id);
        }

        self.frames.extend(input.iter().copied());
        self.expected_input_frame_id =
            Some(first_frame_id.saturating_add(input.len() as u64));
    }

    fn pop_10ms(&mut self) -> Option<(u64, [[f32; 2]; 480])> {
        const N: usize = 480;
        if self.frames.len() < N {
            return None;
        }

        let first_frame_id = self.buffer_start_frame_id?;
        let mut block = [[0.0f32; 2]; N];
        for frame in &mut block {
            *frame = self.frames.pop_front().expect("length checked");
        }

        self.buffer_start_frame_id =
            Some(first_frame_id.saturating_add(N as u64));
        Some((first_frame_id, block))
    }
}

fn float_to_i16(sample: f32, gain: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }

    let scaled = sample.clamp(-1.0, 1.0) * gain.clamp(0.0, 4.0);
    (scaled.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

#[cfg(target_os = "windows")]
pub fn run_udp_send_live(
    config: UdpSendLiveConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        net::UdpSocket,
        sync::mpsc::RecvTimeoutError,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use bumble_codecs::lc3::{
        Lc3Encoder, Lc3FrameDuration, Lc3StreamConfig,
    };

    use crate::{
        plane::{
            l1_media::source::process_loopback::ProcessLoopbackSource,
            l2_transport::protocol::{
                LC3_FRAME_SAMPLES,
                LC3_OCTETS_PER_CHANNEL,
                SAMPLE_RATE_HZ,
                encode_lc3_stereo,
            },
        },
        platform::windows::wasapi::default_render_name,
    };

    let default_render = default_render_name()?;
    if !default_render
        .to_lowercase()
        .contains(&config.sacrificial_substring.to_lowercase())
    {
        return Err(format!(
            "default render endpoint is not the requested sacrificial sink: expected substring {:?}, actual {:?}",
            config.sacrificial_substring,
            default_render,
        )
        .into());
    }

    println!(
        "[live-source] sacrificial default render: {}",
        default_render
    );

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(&config.target)?;

    let session_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as u64
        ^ ((std::process::id() as u64) << 32);

    let codec = Lc3StreamConfig {
        sampling_frequency: SAMPLE_RATE_HZ,
        frame_duration: Lc3FrameDuration::TenMs,
        channels: 1,
        octets_per_codec_frame: LC3_OCTETS_PER_CHANNEL,
        codec_frames_per_sdu: 1,
    };

    debug_assert_eq!(codec.frame_samples(), LC3_FRAME_SAMPLES);

    let left_encoder = Lc3Encoder::new(codec)?;
    let right_encoder = Lc3Encoder::new(codec)?;

    // 16 packets is intentionally modest: capture must never be allowed to
    // build seconds of hidden latency behind a stalled encoder/network path.
    let source = ProcessLoopbackSource::start_excluding_current_process_tree(16)
        .map_err(|error| {
            format!("Windows Process Loopback startup failed: {error:?}")
        })?;

    println!(
        "[udp-send-live] target={} codec=LC3 48k/10ms/{}Bch gain={:.3} session={}",
        config.target,
        LC3_OCTETS_PER_CHANNEL,
        config.gain,
        session_id,
    );

    let started = Instant::now();
    let deadline = started + Duration::from_secs(config.seconds);
    let mut last_report = Instant::now();

    let mut assembler = FrameAssembler::default();
    let mut sequence = 0u64;
    let mut packets_sent = 0u64;
    let mut bytes_sent = 0u64;

    while Instant::now() < deadline {
        match source.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                assembler.push(
                    chunk.first_frame_id,
                    &chunk.frames,
                    chunk.discontinuity,
                );

                while let Some((first_frame_id, pcm)) = assembler.pop_10ms() {
                    let mut left_pcm = [0i16; LC3_FRAME_SAMPLES];
                    let mut right_pcm = [0i16; LC3_FRAME_SAMPLES];

                    for (index, frame) in pcm.iter().enumerate() {
                        left_pcm[index] =
                            float_to_i16(frame[0], config.gain);
                        right_pcm[index] =
                            float_to_i16(frame[1], config.gain);
                    }

                    let left = left_encoder.encode_sdu(&left_pcm)?;
                    let right = right_encoder.encode_sdu(&right_pcm)?;

                    let bytes = encode_lc3_stereo(
                        session_id,
                        sequence,
                        first_frame_id,
                        &left,
                        &right,
                    )?;

                    let sent = socket.send(&bytes)?;
                    if sent != bytes.len() {
                        return Err(format!(
                            "partial UDP send: {} of {} bytes",
                            sent,
                            bytes.len()
                        )
                        .into());
                    }

                    sequence = sequence.saturating_add(1);
                    packets_sent = packets_sent.saturating_add(1);
                    bytes_sent = bytes_sent.saturating_add(sent as u64);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("process-loopback source queue disconnected".into());
            }
        }

        if last_report.elapsed() >= Duration::from_secs(2) {
            let stats = source.stats();
            println!(
                "[udp-send-live] packets={} bytes={} captured={} queue_drop={} discontinuities={} assembler_resets={} devpos={} qpc={}",
                packets_sent,
                bytes_sent,
                stats.captured_frames,
                stats.queue_dropped_frames,
                stats.discontinuities,
                assembler.resets,
                stats.last_device_position,
                stats.last_qpc_position,
            );
            last_report = Instant::now();
        }
    }

    let (source_stats, capture_stats) = source.shutdown().map_err(|error| {
        format!("Windows Process Loopback shutdown failed: {error:?}")
    })?;

    println!(
        "[done] packets={} bytes={} captured={} queue_drop={} discontinuities={} capture_packets={} capture_frames={} silent_frames={} platform_discontinuities={} assembler_resets={}",
        packets_sent,
        bytes_sent,
        source_stats.captured_frames,
        source_stats.queue_dropped_frames,
        source_stats.discontinuities,
        capture_stats.packets,
        capture_stats.frames,
        capture_stats.silent_frames,
        capture_stats.discontinuities,
        assembler.resets,
    );

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn run_udp_send_live(
    _config: UdpSendLiveConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("udp-send-live is only available on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_preserves_absolute_frame_identity() {
        let mut assembler = FrameAssembler::default();
        let first = vec![[0.25, -0.25]; 240];
        let second = vec![[0.5, -0.5]; 240];

        assembler.push(1_000, &first, false);
        assert!(assembler.pop_10ms().is_none());

        assembler.push(1_240, &second, false);
        let (frame_id, block) = assembler.pop_10ms().unwrap();

        assert_eq!(frame_id, 1_000);
        assert_eq!(block[0], [0.25, -0.25]);
        assert_eq!(block[479], [0.5, -0.5]);
    }

    #[test]
    fn assembler_drops_partial_history_on_source_gap() {
        let mut assembler = FrameAssembler::default();
        assembler.push(0, &vec![[0.1, 0.1]; 240], false);
        assembler.push(960, &vec![[0.2, 0.2]; 480], false);

        let (frame_id, block) = assembler.pop_10ms().unwrap();
        assert_eq!(frame_id, 960);
        assert_eq!(block[0], [0.2, 0.2]);
        assert_eq!(assembler.resets, 1);
    }
}
