//! Windows -> Android first-sound HIL using a deterministic PCM tone.

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

    use crate::plane::l2_transport::protocol::{
        MAX_PACKET_FRAMES,
        SAMPLE_RATE_HZ,
        encode_pcm16,
    };

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(&config.target)?;

    let session_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as u64
        ^ ((std::process::id() as u64) << 32);

    let packet_duration = Duration::from_secs_f64(
        MAX_PACKET_FRAMES as f64 / SAMPLE_RATE_HZ as f64,
    );

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
        "[udp-send-test] target={} tone={:.1}Hz gain={:.3} packet={}f/{:.1}ms session={}",
        config.target,
        config.tone_hz,
        gain,
        MAX_PACKET_FRAMES,
        packet_duration.as_secs_f64() * 1000.0,
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

        let mut frames = Vec::with_capacity(MAX_PACKET_FRAMES);
        for _ in 0..MAX_PACKET_FRAMES {
            let sample = (phase.sin() * gain * i16::MAX as f64)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16;

            frames.push([sample, sample]);

            phase += phase_step;
            if phase >= TAU {
                phase -= TAU;
            }
        }

        let bytes = encode_pcm16(
            session_id,
            sequence,
            first_frame_id,
            &frames,
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
        first_frame_id += MAX_PACKET_FRAMES as u64;

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
