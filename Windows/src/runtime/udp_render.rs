//! End-to-end network HIL: Android UDP PCM -> reorder -> ASRC -> WASAPI.

#[derive(Clone, Debug)]
pub struct UdpRenderConfig {
    pub bind: String,
    pub device_substring: Option<String>,
    pub seconds: u64,
}

#[cfg(target_os = "windows")]
pub fn run_udp_render(
    config: UdpRenderConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use crate::{
        plane::{
            l1_media::{
                asrc::{
                    runtime::AsrcRuntime,
                    servo::{ClockServo, ClockServoOutput},
                },
                pcm::ring::SourcePcmRing,
            },
            l2_transport::receiver::UdpReceiver,
        },
        platform::windows::wasapi::WasapiRenderSink,
    };

    const RING_CAPACITY_FRAMES: usize = 32_768;
    const OUTPUT_RATE_HZ: f64 = 48_000.0;

    let sink =
        WasapiRenderSink::open(config.device_substring.as_deref())?;
    let initial_wasapi_frames = sink.available_frames()?;

    let ring = Arc::new(SourcePcmRing::new(RING_CAPACITY_FRAMES)?);
    let receiver = UdpReceiver::start(&config.bind, Arc::clone(&ring))?;

    println!(
        "[udp] listening on {} | waiting for Android PCM",
        receiver.local_addr
    );

    let mut asrc = AsrcRuntime::reference();
    let mut servo = ClockServo::reference();
    let mut servo_out = ClockServoOutput {
        step: 1.0,
        ..ClockServoOutput::default()
    };

    let startup_target = servo
        .target_fill_frames()
        .saturating_add(initial_wasapi_frames)
        .saturating_add(asrc.minimum_source_frames());

    let startup_deadline = Instant::now() + Duration::from_secs(30);
    while ring.available_frames() < startup_target {
        if Instant::now() >= startup_deadline {
            let stats = receiver.stats();
            return Err(format!(
                "UDP source did not prime in 30 s (packets={}, decode_errors={})",
                stats.packets_received,
                stats.decode_errors,
            )
            .into());
        }

        thread::sleep(Duration::from_millis(1));
    }

    println!(
        "[udp] primed source FIFO={} target={} initial_wasapi={}",
        ring.available_frames(),
        servo.target_fill_frames(),
        initial_wasapi_frames,
    );

    let mut total_output = 0u64;
    let mut total_underflow = 0u64;
    let mut total_retired = 0u64;

    if initial_wasapi_frames > 0 {
        let mut frames = vec![[0.0f32; 2]; initial_wasapi_frames];
        let result = asrc.render(&ring, &mut frames, servo_out.step);

        total_output += initial_wasapi_frames as u64;
        total_underflow += result.underflow_frames as u64;
        total_retired += result.retired_source_frames as u64;

        servo_out = servo.update_after_render(
            ring.available_frames(),
            initial_wasapi_frames,
            OUTPUT_RATE_HZ,
        );

        sink.write_float_stereo(&frames)?;
    }

    sink.audio_client.start_stream()?;

    let started = Instant::now();
    let mut last_report = Instant::now();

    while started.elapsed() < Duration::from_secs(config.seconds) {
        sink.event.wait_for_event(1_000)?;

        let available = sink.available_frames()?;
        if available == 0 {
            continue;
        }

        let mut frames = vec![[0.0f32; 2]; available];
        let result =
            asrc.render(&ring, &mut frames, servo_out.step);

        total_output += available as u64;
        total_underflow += result.underflow_frames as u64;
        total_retired += result.retired_source_frames as u64;

        servo_out = servo.update_after_render(
            ring.available_frames(),
            available,
            OUTPUT_RATE_HZ,
        );

        sink.write_float_stereo(&frames)?;

        if last_report.elapsed() >= Duration::from_secs(2) {
            let net = receiver.stats();

            println!(
                "[udp] packets={} seq={} frame={} lost={} reorder={} decode={} conceal={} \
                 ring={} filtered={:.1} total={:+.1}ppm drift={:+.1}ppm \
                 underflow={} ring_drop={}",
                net.packets_received,
                net.last_sequence,
                net.last_first_frame_id,
                net.lost_packets,
                net.reordered_arrivals,
                net.decode_errors,
                net.concealed_gap_frames,
                ring.available_frames(),
                servo_out.filtered_fill_frames,
                servo_out.correction_ppm,
                servo_out.drift_estimate_ppm,
                total_underflow,
                net.ring_dropped_frames,
            );

            last_report = Instant::now();
        }
    }

    sink.audio_client.stop_stream()?;
    let final_net = receiver.stop();

    println!(
        "[done] output={} underflow={} retired={} packets={} lost={} reorder={} decode={} conceal={} ring_drop={}",
        total_output,
        total_underflow,
        total_retired,
        final_net.packets_received,
        final_net.lost_packets,
        final_net.reordered_arrivals,
        final_net.decode_errors,
        final_net.concealed_gap_frames,
        final_net.ring_dropped_frames,
    );

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn run_udp_render(
    _config: UdpRenderConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("udp-render is only available on Windows".into())
}
