//! Windows -> Android first-sound HIL using LC3-over-UDP v2.

#[derive(Clone, Debug)]
pub struct UdpSendTestConfig {
    pub target: String,
    pub seconds: u64,
    pub tone_hz: f64,
    pub gain: f64,
}

pub fn run_udp_send_test(
    config: UdpSendTestConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        f64::consts::TAU,
        net::UdpSocket,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use bumble_codecs::lc3::{
        Lc3Encoder, Lc3FrameDuration, Lc3StreamConfig,
    };

    use crate::plane::l2_transport::protocol::{
        LC3_FRAME_SAMPLES,
        LC3_OCTETS_PER_CHANNEL,
        SAMPLE_RATE_HZ,
        encode_lc3_stereo,
    };

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

    let packet_duration = Duration::from_micros(10_000);

    let started = Instant::now();
    let deadline = started + Duration::from_secs(config.seconds);
    let mut next_send = started;

    let gain = config.gain.clamp(0.0, 1.0);
    let phase_step = TAU * config.tone_hz / SAMPLE_RATE_HZ as f64;
    let mut phase = 0.0f64;

    let mut sequence = 0u64;
    let mut first_frame_id = 0u64;
    let mut packets_sent = 0u64;
    let mut bytes_sent = 0u64;
    let mut max_late_us = 0u128;
    let mut last_report = Instant::now();

    println!(
        "[udp-send-test] target={} codec=LC3 48k/10ms/{}Bch tone={:.1}Hz gain={:.3} packet=10.0ms session={}",
        config.target,
        LC3_OCTETS_PER_CHANNEL,
        config.tone_hz,
        gain,
        session_id,
    );

    while Instant::now() < deadline {
        let now = Instant::now();
        if now < next_send {
            thread::sleep(next_send - now);
        } else {
            max_late_us = max_late_us.max(
                now.duration_since(next_send).as_micros(),
            );
        }

        let mut left_pcm = [0i16; LC3_FRAME_SAMPLES];
        let mut right_pcm = [0i16; LC3_FRAME_SAMPLES];

        for i in 0..LC3_FRAME_SAMPLES {
            let sample = (phase.sin() * gain * i16::MAX as f64)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16;

            left_pcm[i] = sample;
            right_pcm[i] = sample;

            phase += phase_step;
            if phase >= TAU {
                phase -= TAU;
            }
        }

        let left = left_encoder.encode_sdu(&left_pcm)?;
        let right = right_encoder.encode_sdu(&right_pcm)?;

        if left.len() != LC3_OCTETS_PER_CHANNEL
            || right.len() != LC3_OCTETS_PER_CHANNEL
        {
            return Err(format!(
                "LC3 encoded size mismatch: left={} right={} expected={}",
                left.len(),
                right.len(),
                LC3_OCTETS_PER_CHANNEL
            )
            .into());
        }

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

        packets_sent += 1;
        bytes_sent += sent as u64;
        sequence += 1;
        first_frame_id += LC3_FRAME_SAMPLES as u64;

        // Preserve the synthetic source timeline across scheduler stalls.
        next_send += packet_duration;

        if last_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "[udp-send-test] packets={} frame={} bytes={} max_late={}us",
                packets_sent,
                first_frame_id,
                bytes_sent,
                max_late_us,
            );
            last_report = Instant::now();
        }
    }

    println!(
        "[done] packets={} frames={} bytes={} max_late={}us",
        packets_sent,
        first_frame_id,
        bytes_sent,
        max_late_us,
    );

    Ok(())
}
