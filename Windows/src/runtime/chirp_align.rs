//! Alternating acoustic chirp measurement:
//! Realtek -> S24 -> Realtek -> S24, five pairs.
//!
//! The source timeline is common to both branches. Microphone capture has an
//! arbitrary constant offset, but that cancels when comparing branch medians.

#[cfg(target_os = "windows")]
use std::{
    collections::VecDeque,
    f64::consts::PI,
    net::UdpSocket,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "windows")]
use bumble_codecs::lc3::{
    Lc3Encoder, Lc3FrameDuration, Lc3StreamConfig,
};

#[cfg(target_os = "windows")]
use crate::{
    plane::l2_transport::protocol::{
        encode_lc3_stereo, LC3_FRAME_SAMPLES, LC3_OCTETS_PER_CHANNEL,
        SAMPLE_RATE_HZ,
    },
    platform::windows::{
        mic_capture::MicCapture,
        wasapi::WasapiRenderSink,
    },
};

const BLOCK_FRAMES: usize = 480;
const RUNS: usize = 10;
const WARMUP_MS: usize = 3_000;
const INTERVAL_MS: usize = 1_000;
const CHIRP_MS: usize = 60;
const TAIL_MS: usize = 800;
const MIC_PREROLL_MS: usize = 500;
const SEARCH_EARLY_MS: usize = 100;
const SEARCH_LATE_MS: usize = 700;
const CORRELATION_DECIMATION: usize = 4;
const MIN_CORRELATION: f64 = 0.08;
const LOCAL_QUEUE_BLOCKS: usize = 32;
const LOCAL_CUSHION_FRAMES: usize = 1_920;

const CHIRP_START_HZ: f64 = 1_200.0;
const CHIRP_END_HZ: f64 = 9_000.0;
const CHIRP_AMPLITUDE: f32 = 0.25;

#[derive(Clone, Debug)]
pub struct ChirpAlignConfig {
    pub target: String,
    pub local_device_substring: String,
    pub mic_device_substring: String,
}

#[derive(Clone)]
struct LocalBlock {
    frames: [[f32; 2]; BLOCK_FRAMES],
}

#[derive(Clone, Copy, Debug)]
enum Branch {
    Realtek,
    S24,
}

impl Branch {
    fn label(self) -> &'static str {
        match self {
            Self::Realtek => "Realtek",
            Self::S24 => "S24",
        }
    }
}

fn generate_reference_chirp() -> Vec<f32> {
    let frames = SAMPLE_RATE_HZ as usize * CHIRP_MS / 1_000;
    let duration = frames as f64 / SAMPLE_RATE_HZ as f64;
    let slope = (CHIRP_END_HZ - CHIRP_START_HZ) / duration;

    (0..frames)
        .map(|index| {
            let t = index as f64 / SAMPLE_RATE_HZ as f64;
            let phase = 2.0
                * PI
                * (CHIRP_START_HZ * t + 0.5 * slope * t * t);
            let window = if frames <= 1 {
                1.0
            } else {
                0.5
                    - 0.5
                        * (2.0 * PI * index as f64 / (frames - 1) as f64)
                            .cos()
            };
            CHIRP_AMPLITUDE * (phase.sin() * window) as f32
        })
        .collect()
}

fn fill_test_block(
    source_start_frame: usize,
    chirp: &[f32],
) -> (
    [[f32; 2]; BLOCK_FRAMES],
    [[f32; 2]; BLOCK_FRAMES],
) {
    let mut local = [[0.0f32; 2]; BLOCK_FRAMES];
    let mut s24 = [[0.0f32; 2]; BLOCK_FRAMES];

    let block_end = source_start_frame + BLOCK_FRAMES;
    let warmup_frames = SAMPLE_RATE_HZ as usize * WARMUP_MS / 1_000;
    let interval_frames = SAMPLE_RATE_HZ as usize * INTERVAL_MS / 1_000;

    for run in 0..RUNS {
        let chirp_start = warmup_frames + run * interval_frames;
        let chirp_end = chirp_start + chirp.len();

        let overlap_start = source_start_frame.max(chirp_start);
        let overlap_end = block_end.min(chirp_end);
        if overlap_start >= overlap_end {
            continue;
        }

        for global in overlap_start..overlap_end {
            let block_index = global - source_start_frame;
            let chirp_index = global - chirp_start;
            let sample = chirp[chirp_index];
            let frame = [sample, sample];

            if run % 2 == 0 {
                local[block_index] = frame;
            } else {
                s24[block_index] = frame;
            }
        }
    }

    (local, s24)
}

fn float_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

fn run_local_render(
    device_substring: String,
    receiver: Receiver<LocalBlock>,
) -> Result<(), String> {
    let sink = WasapiRenderSink::open(Some(&device_substring))
        .map_err(|error| format!("local WASAPI open failed: {error}"))?;
    let initial_wasapi_frames = sink
        .available_frames()
        .map_err(|error| format!("local available_frames failed: {error}"))?;

    println!(
        "[chirp][local] device={} initial_wasapi={} cushion={}f",
        sink.device_name,
        initial_wasapi_frames,
        LOCAL_CUSHION_FRAMES,
    );

    let mut fifo = VecDeque::<[f32; 2]>::new();
    let prime_target =
        initial_wasapi_frames.saturating_add(LOCAL_CUSHION_FRAMES);
    let mut disconnected = false;

    while fifo.len() < prime_target && !disconnected {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(block) => fifo.extend(block.frames),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                disconnected = true;
            }
        }
    }

    if fifo.len() < initial_wasapi_frames {
        return Err("local branch did not prime".into());
    }

    if initial_wasapi_frames > 0 {
        let mut initial = Vec::with_capacity(initial_wasapi_frames);
        for _ in 0..initial_wasapi_frames {
            initial.push(fifo.pop_front().unwrap_or([0.0, 0.0]));
        }
        sink.write_float_stereo(&initial)
            .map_err(|error| format!("local initial write failed: {error}"))?;
    }

    sink.audio_client
        .start_stream()
        .map_err(|error| format!("local stream start failed: {error}"))?;

    loop {
        if sink.event.wait_for_event(1_000).is_err() {
            if disconnected && fifo.is_empty() {
                break;
            }
            continue;
        }

        loop {
            match receiver.try_recv() {
                Ok(block) => fifo.extend(block.frames),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        let available = sink
            .available_frames()
            .map_err(|error| format!("local available_frames failed: {error}"))?;
        if available == 0 {
            if disconnected && fifo.is_empty() {
                break;
            }
            continue;
        }

        let mut output = Vec::with_capacity(available);
        for _ in 0..available {
            output.push(fifo.pop_front().unwrap_or([0.0, 0.0]));
        }

        sink.write_float_stereo(&output)
            .map_err(|error| format!("local write failed: {error}"))?;

        if disconnected && fifo.is_empty() {
            break;
        }
    }

    let _ = sink.audio_client.stop_stream();
    Ok(())
}

fn send_local(
    sender: &SyncSender<LocalBlock>,
    frames: [[f32; 2]; BLOCK_FRAMES],
) -> Result<(), String> {
    sender
        .try_send(LocalBlock { frames })
        .map_err(|error| match error {
            TrySendError::Full(_) => {
                "local chirp render queue overflowed".to_string()
            }
            TrySendError::Disconnected(_) => {
                "local chirp render queue disconnected".to_string()
            }
        })
}

fn normalized_correlation_at(
    captured: &[f32],
    reference: &[f32],
    start: usize,
    step: usize,
) -> f64 {
    let mut xy = 0.0f64;
    let mut xx = 0.0f64;
    let mut yy = 0.0f64;

    for index in (0..reference.len()).step_by(step) {
        let x = reference[index] as f64;
        let y = captured[start + index] as f64;
        xy += x * y;
        xx += x * x;
        yy += y * y;
    }

    if xx <= 1.0e-12 || yy <= 1.0e-12 {
        0.0
    } else {
        (xy / (xx * yy).sqrt()).abs()
    }
}

fn find_best_correlation(
    captured: &[f32],
    reference: &[f32],
    search_start: usize,
    search_end: usize,
) -> Result<(usize, f64), String> {
    if captured.len() < reference.len() {
        return Err("microphone recording is shorter than chirp reference".into());
    }

    let max_start = captured.len().saturating_sub(reference.len());
    let start = search_start.min(max_start);
    let end = search_end.min(max_start);
    if end <= start {
        return Err("correlation search window is empty".into());
    }

    let mut best_frame = start;
    let mut best_score = -1.0f64;

    for frame in (start..=end).step_by(CORRELATION_DECIMATION) {
        let score = normalized_correlation_at(
            captured,
            reference,
            frame,
            CORRELATION_DECIMATION,
        );
        if score > best_score {
            best_score = score;
            best_frame = frame;
        }
    }

    let refine_start = best_frame.saturating_sub(CORRELATION_DECIMATION);
    let refine_end =
        (best_frame + CORRELATION_DECIMATION).min(end);

    for frame in refine_start..=refine_end {
        let score = normalized_correlation_at(
            captured,
            reference,
            frame,
            CORRELATION_DECIMATION,
        );
        if score > best_score {
            best_score = score;
            best_frame = frame;
        }
    }

    Ok((best_frame, best_score))
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        Some((values[mid - 1] + values[mid]) * 0.5)
    }
}

#[cfg(target_os = "windows")]
pub fn run_chirp_align(
    config: ChirpAlignConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let chirp = generate_reference_chirp();

    let (local_tx, local_rx) =
        mpsc::sync_channel::<LocalBlock>(LOCAL_QUEUE_BLOCKS);
    let local_device = config.local_device_substring.clone();
    let local_join = thread::Builder::new()
        .name("s24-chirp-local-render".into())
        .spawn(move || run_local_render(local_device, local_rx))
        .map_err(|error| format!("cannot spawn local chirp render: {error}"))?;

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
    let left_encoder = Lc3Encoder::new(codec)?;
    let right_encoder = Lc3Encoder::new(codec)?;

    let mic = MicCapture::start(&config.mic_device_substring)
        .map_err(|error| format!("microphone startup failed: {error}"))?;
    thread::sleep(Duration::from_millis(MIC_PREROLL_MS as u64));

    let total_ms =
        WARMUP_MS + (RUNS - 1) * INTERVAL_MS + CHIRP_MS + TAIL_MS;
    let total_frames = SAMPLE_RATE_HZ as usize * total_ms / 1_000;
    let total_blocks = total_frames.div_ceil(BLOCK_FRAMES);

    println!(
        "[chirp] sequence=R-S x5 mic={} local={} target={}",
        config.mic_device_substring,
        config.local_device_substring,
        config.target,
    );
    println!(
        "[chirp] signal={}ms {:.0}->{:.0}Hz amp={:.2} warmup={}ms interval={}ms",
        CHIRP_MS,
        CHIRP_START_HZ,
        CHIRP_END_HZ,
        CHIRP_AMPLITUDE,
        WARMUP_MS,
        INTERVAL_MS,
    );

    let started = Instant::now();
    let mut next_deadline = started;
    let block_period = Duration::from_millis(10);
    let mut sequence = 0u64;
    let mut max_late_us = 0u128;

    for block_index in 0..total_blocks {
        let source_start = block_index * BLOCK_FRAMES;
        let (local, s24) = fill_test_block(source_start, &chirp);

        send_local(&local_tx, local)?;

        let mut left_pcm = [0i16; LC3_FRAME_SAMPLES];
        let mut right_pcm = [0i16; LC3_FRAME_SAMPLES];
        for (index, frame) in s24.iter().enumerate() {
            left_pcm[index] = float_to_i16(frame[0]);
            right_pcm[index] = float_to_i16(frame[1]);
        }

        let left = left_encoder.encode_sdu(&left_pcm)?;
        let right = right_encoder.encode_sdu(&right_pcm)?;
        let bytes = encode_lc3_stereo(
            session_id,
            sequence,
            source_start as u64,
            &left,
            &right,
        )?;
        socket.send(&bytes)?;

        sequence = sequence.saturating_add(1);
        next_deadline += block_period;

        let now = Instant::now();
        if next_deadline > now {
            thread::sleep(next_deadline - now);
        } else {
            max_late_us = max_late_us.max(
                now.duration_since(next_deadline).as_micros(),
            );
        }
    }

    drop(local_tx);

    let local_result = local_join
        .join()
        .map_err(|_| "local chirp render thread panicked")?;
    if let Err(error) = local_result {
        return Err(error.into());
    }

    thread::sleep(Duration::from_millis(250));
    let capture = mic
        .stop()
        .map_err(|error| format!("microphone stop failed: {error}"))?;

    println!(
        "[chirp] captured={} frames / {:.2}s generator_max_late={}us",
        capture.samples.len(),
        capture.samples.len() as f64 / capture.sample_rate_hz as f64,
        max_late_us,
    );

    let warmup_frames = SAMPLE_RATE_HZ as usize * WARMUP_MS / 1_000;
    let interval_frames = SAMPLE_RATE_HZ as usize * INTERVAL_MS / 1_000;
    let preroll_frames = SAMPLE_RATE_HZ as usize * MIC_PREROLL_MS / 1_000;
    let search_early = SAMPLE_RATE_HZ as usize * SEARCH_EARLY_MS / 1_000;
    let search_late = SAMPLE_RATE_HZ as usize * SEARCH_LATE_MS / 1_000;

    let mut realtek_offsets = Vec::<f64>::new();
    let mut s24_offsets = Vec::<f64>::new();

    println!();
    println!("Acoustic chirp hits:");

    for run in 0..RUNS {
        let branch = if run % 2 == 0 {
            Branch::Realtek
        } else {
            Branch::S24
        };

        let source_frame = warmup_frames + run * interval_frames;
        let expected_capture_frame = preroll_frames + source_frame;
        let search_start =
            expected_capture_frame.saturating_sub(search_early);
        let search_end =
            expected_capture_frame.saturating_add(search_late);

        let (hit_frame, score) = find_best_correlation(
            &capture.samples,
            &chirp,
            search_start,
            search_end,
        )?;

        let approximate_latency_ms =
            (hit_frame as f64
                - preroll_frames as f64
                - source_frame as f64)
                * 1_000.0
                / SAMPLE_RATE_HZ as f64;

        println!(
            "#{:02} {:7} hit={:7} approx={:8.2} ms corr={:.3}{}",
            run + 1,
            branch.label(),
            hit_frame,
            approximate_latency_ms,
            score,
            if score < MIN_CORRELATION {
                "  REJECT"
            } else {
                ""
            },
        );

        if score >= MIN_CORRELATION {
            let offset_frames =
                hit_frame as f64 - source_frame as f64;
            match branch {
                Branch::Realtek => realtek_offsets.push(offset_frames),
                Branch::S24 => s24_offsets.push(offset_frames),
            }
        }
    }

    if realtek_offsets.len() < 3 || s24_offsets.len() < 3 {
        return Err(format!(
            "not enough valid chirps: Realtek={} S24={} (need >=3 each)",
            realtek_offsets.len(),
            s24_offsets.len(),
        )
        .into());
    }

    let realtek_median_frames =
        median(realtek_offsets).expect("validated nonempty");
    let s24_median_frames =
        median(s24_offsets).expect("validated nonempty");

    let realtek_median_ms =
        (realtek_median_frames - preroll_frames as f64)
            * 1_000.0
            / SAMPLE_RATE_HZ as f64;
    let s24_median_ms =
        (s24_median_frames - preroll_frames as f64)
            * 1_000.0
            / SAMPLE_RATE_HZ as f64;

    let delta_ms =
        (s24_median_frames - realtek_median_frames)
            * 1_000.0
            / SAMPLE_RATE_HZ as f64;

    println!();
    println!("Relative acoustic latency:");
    println!("  Realtek median : {:8.2} ms", realtek_median_ms);
    println!("  S24 median     : {:8.2} ms", s24_median_ms);
    println!("  S24 - Realtek  : {:+8.2} ms", delta_ms);

    if delta_ms >= 0.0 {
        println!(
            "  Align           : Realtek delay +{:.2} ms, S24 delay +0 ms",
            delta_ms,
        );
    } else {
        println!(
            "  Align           : Realtek delay +0 ms, S24 delay +{:.2} ms",
            -delta_ms,
        );
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn run_chirp_align(
    _config: ChirpAlignConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("chirp-align is only available on Windows".into())
}
