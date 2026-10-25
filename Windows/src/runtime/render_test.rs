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
            asrc::{
                runtime::AsrcRuntime,
                servo::{ClockServo, ClockServoOutput},
            },
            pcm::ring::SourcePcmRing,
            source::synthetic::SyntheticProducer,
        },
        platform::windows::wasapi::WasapiRenderSink,
    };

    const RING_CAPACITY_FRAMES: usize = 16_384;
    const OUTPUT_RATE_HZ: f64 = 48_000.0;

    let sink =
        WasapiRenderSink::open(config.device_substring.as_deref())?;
    let initial_wasapi_frames = sink.available_frames()?;

    let ring = Arc::new(SourcePcmRing::new(RING_CAPACITY_FRAMES)?);
    let producer = SyntheticProducer::start(
        Arc::clone(&ring),
        config.tone_hz,
        0.08,
        config.source_ppm,
    );

    let mut asrc = AsrcRuntime::reference();
    let mut servo = ClockServo::reference();
    let mut servo_out = ClockServoOutput {
        step: 1.0,
        ..ClockServoOutput::default()
    };

    // Prime enough source to fill WASAPI once and still retain the servo's
    // post-render target cushion plus the FIR lookahead/history requirement.
    let startup_target = servo
        .target_fill_frames()
        .saturating_add(initial_wasapi_frames)
        .saturating_add(asrc.minimum_source_frames());

    let startup_deadline = Instant::now() + Duration::from_secs(2);
    while ring.available_frames() < startup_target {
        if Instant::now() >= startup_deadline {
            return Err("synthetic source failed to prime ASRC FIFO".into());
        }
        thread::sleep(Duration::from_millis(1));
    }

    println!(
        "[asrc] primed source FIFO={} residual_target={} wasapi_initial={} \
         source_ppm={:+.3}",
        ring.available_frames(),
        servo.target_fill_frames(),
        initial_wasapi_frames,
        config.source_ppm
    );

    let mut total_output = 0u64;
    let mut total_underflow = 0u64;
    let mut total_retired = 0u64;
    let mut last_report = Instant::now();

    if initial_wasapi_frames > 0 {
        let mut frames = vec![[0.0f32; 2]; initial_wasapi_frames];
        let result =
            asrc.render(&ring, &mut frames, servo_out.step);

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

    // Start rate observation only after the initial WASAPI prefill has been
    // committed. Otherwise the output baseline contains one buffer of audio
    // that the source-progress baseline never saw, creating a large fake
    // negative ppm bias that only decays slowly with runtime.
    let source_baseline = producer.produced_frames();
    let output_baseline = total_output;
    let started = Instant::now();

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
            let source_progress =
                producer.produced_frames().saturating_sub(source_baseline);
            let output_progress =
                total_output.saturating_sub(output_baseline);
            let observed_ratio_ppm =
                if output_progress > 0 {
                    (source_progress as f64 / output_progress as f64 - 1.0)
                        * 1.0e6
                } else {
                    0.0
                };

            println!(
                "[asrc] raw={} filtered={:.1} err={:+.1} slope={:+.1}f/s step={:.9} \
                 total={:+.2}ppm drift={:+.2}ppm buffer={:+.2}ppm observed={:+.1}ppm \
                 acquiring={} stable={:.2}s underflow={} retired={} producer_drop={} \
                 producer_max_late_us={}",
                servo_out.raw_fill_frames,
                servo_out.filtered_fill_frames,
                servo_out.fill_error_frames,
                servo_out.filtered_fill_slope_frames_per_second,
                servo_out.step,
                servo_out.correction_ppm,
                servo_out.drift_estimate_ppm,
                servo_out.buffer_correction_ppm,
                observed_ratio_ppm,
                servo_out.acquiring,
                servo_out.acquisition_stable_seconds,
                total_underflow,
                total_retired,
                producer.dropped_frames(),
                producer.max_lateness_us(),
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
