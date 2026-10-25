//! Output-first HIL: synthetic independent source -> ASRC -> WASAPI.
//!
//! This intentionally bypasses UDP. It proves the clock-domain crossing and
//! Realtek render path before transport is introduced.

#[derive(Clone, Debug)]
pub struct RenderTestConfig {
    pub device_substring: Option<String>,
    pub seconds: u64,
    pub tone_hz: f64,
    pub source_ppm: f64,
}

#[cfg(target_os = "windows")]
pub fn run_render_test(
    config: RenderTestConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use crate::{
        plane::l1_media::{
            asrc::{runtime::AsrcRuntime, servo::ClockServo},
            pcm::ring::SourcePcmRing,
            source::synthetic::SyntheticProducer,
        },
        platform::windows::wasapi::WasapiRenderSink,
    };

    const RING_CAPACITY_FRAMES: usize = 16_384;
    const OUTPUT_RATE_HZ: f64 = 48_000.0;

    let ring = Arc::new(SourcePcmRing::new(RING_CAPACITY_FRAMES)?);
    let producer = SyntheticProducer::start(
        Arc::clone(&ring),
        config.tone_hz,
        0.08,
        config.source_ppm,
    );

    let mut asrc = AsrcRuntime::reference();
    let mut servo = ClockServo::reference();

    let startup_target = servo
        .target_fill_frames()
        .max(asrc.minimum_source_frames() + 480);

    let startup_deadline = Instant::now() + Duration::from_secs(2);
    while ring.available_frames() < startup_target {
        if Instant::now() >= startup_deadline {
            return Err("synthetic source failed to prime ASRC FIFO".into());
        }
        thread::sleep(Duration::from_millis(1));
    }

    println!(
        "[asrc] primed source FIFO={} target={} source_ppm={:+.3}",
        ring.available_frames(),
        servo.target_fill_frames(),
        config.source_ppm
    );

    let sink =
        WasapiRenderSink::open(config.device_substring.as_deref())?;

    let mut total_output = 0u64;
    let mut total_underflow = 0u64;
    let mut total_retired = 0u64;
    let mut last_report = Instant::now();
    let started = Instant::now();

    let initial = sink.available_frames()?;
    if initial > 0 {
        let servo_out =
            servo.update(ring.available_frames(), initial, OUTPUT_RATE_HZ);
        let mut frames = vec![[0.0f32; 2]; initial];
        let result =
            asrc.render(&ring, &mut frames, servo_out.step);
        total_output += initial as u64;
        total_underflow += result.underflow_frames as u64;
        total_retired += result.retired_source_frames as u64;
        sink.write_float_stereo(&frames)?;
    }

    sink.audio_client.start_stream()?;

    while started.elapsed() < Duration::from_secs(config.seconds) {
        sink.event.wait_for_event(1_000)?;

        let available = sink.available_frames()?;
        if available == 0 {
            continue;
        }

        let servo_out =
            servo.update(ring.available_frames(), available, OUTPUT_RATE_HZ);
        let mut frames = vec![[0.0f32; 2]; available];
        let result =
            asrc.render(&ring, &mut frames, servo_out.step);

        total_output += available as u64;
        total_underflow += result.underflow_frames as u64;
        total_retired += result.retired_source_frames as u64;

        sink.write_float_stereo(&frames)?;

        if last_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "[asrc] fill={} err={:+.0} step={:.9} correction={:+.2}ppm \
                 underflow={} retired={} producer_drop={}",
                ring.available_frames(),
                servo_out.fill_error_frames,
                servo_out.step,
                servo_out.correction_ppm,
                total_underflow,
                total_retired,
                producer.dropped_frames(),
            );
            last_report = Instant::now();
        }
    }

    sink.audio_client.stop_stream()?;
    let producer_drop = producer.stop();

    println!(
        "[done] output={} underflow={} retired={} producer_drop={}",
        total_output, total_underflow, total_retired, producer_drop
    );
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn run_render_test(
    _config: RenderTestConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("render-test is only available on Windows".into())
}
