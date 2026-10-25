#![cfg(windows)]

//! Minimal manual dual-output alignment surface.
//!
//! Windows Process Loopback is the single master PCM source:
//!   branch A -> adjustable delay -> LC3 -> UDP -> S24
//!   branch B -> bounded queue -> adjustable delay/gain -> WASAPI -> Realtek
//!
//! The GUI owns only atomics. Audio/network threads keep stdout telemetry.

use std::{
    collections::VecDeque,
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bumble_codecs::lc3::{
    Lc3Encoder, Lc3FrameDuration, Lc3StreamConfig,
};
use eframe::egui;

use super::chirp_align::{
    measure_chirp_alignment, ChirpAlignConfig, ChirpAlignment,
};

use crate::{
    plane::{
        l1_media::source::process_loopback::ProcessLoopbackSource,
        l2_transport::protocol::{
            encode_lc3_stereo, LC3_FRAME_SAMPLES,
            LC3_OCTETS_PER_CHANNEL, SAMPLE_RATE_HZ,
        },
    },
    platform::windows::wasapi::{
        default_render_name, WasapiRenderSink,
    },
};

const BLOCK_FRAMES: usize = 480;
const SESSION_PREROLL_MS: u64 = 2_000;
const SESSION_PREROLL_PACKETS: u64 = SESSION_PREROLL_MS / 10;
const LOCAL_QUEUE_BLOCKS: usize = 16;
const LOCAL_STARTUP_CUSHION_FRAMES: usize = 1_920;
const MAX_DELAY_MS: u32 = 3_000;
const GUI_REPAINT_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct DualOutputGuiConfig {
    pub target: String,
    pub sacrificial_substring: String,
    pub local_device_substring: String,
    pub mic_device_substring: String,
}

#[derive(Clone)]
struct SharedControls {
    stop: Arc<AtomicBool>,
    local_gain_bits: Arc<AtomicU32>,
    s24_delay_ms: Arc<AtomicU32>,
    local_delay_ms: Arc<AtomicU32>,
}

impl SharedControls {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            local_gain_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            s24_delay_ms: Arc::new(AtomicU32::new(0)),
            local_delay_ms: Arc::new(AtomicU32::new(0)),
        }
    }

    fn local_gain(&self) -> f32 {
        f32::from_bits(self.local_gain_bits.load(Ordering::Relaxed))
            .clamp(0.0, 2.0)
    }

    fn set_local_gain(&self, value: f32) {
        self.local_gain_bits.store(
            value.clamp(0.0, 2.0).to_bits(),
            Ordering::Relaxed,
        );
    }

    fn s24_delay_ms(&self) -> u32 {
        self.s24_delay_ms.load(Ordering::Relaxed).min(MAX_DELAY_MS)
    }

    fn set_s24_delay_ms(&self, value: u32) {
        self.s24_delay_ms
            .store(value.min(MAX_DELAY_MS), Ordering::Relaxed);
    }

    fn local_delay_ms(&self) -> u32 {
        self.local_delay_ms.load(Ordering::Relaxed).min(MAX_DELAY_MS)
    }

    fn set_local_delay_ms(&self, value: u32) {
        self.local_delay_ms
            .store(value.min(MAX_DELAY_MS), Ordering::Relaxed);
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct AdjustableDelay {
    fifo: VecDeque<[f32; 2]>,
}

impl AdjustableDelay {
    fn reset(&mut self) {
        self.fifo.clear();
    }

    fn process(
        &mut self,
        input: &[[f32; 2]],
        delay_ms: u32,
    ) -> Vec<[f32; 2]> {
        let target_frames =
            (delay_ms.min(MAX_DELAY_MS) as usize * SAMPLE_RATE_HZ as usize)
                / 1_000;

        while self.fifo.len() > target_frames {
            self.fifo.pop_front();
        }

        let mut output = Vec::with_capacity(input.len());
        for &frame in input {
            self.fifo.push_back(frame);
            if self.fifo.len() > target_frames {
                output.push(self.fifo.pop_front().unwrap_or([0.0, 0.0]));
            } else {
                output.push([0.0, 0.0]);
            }
        }
        output
    }
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
        let timeline_break = self
            .expected_input_frame_id
            .map(|expected| expected != first_frame_id)
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

    fn pop_block(
        &mut self,
    ) -> Option<(u64, [[f32; 2]; BLOCK_FRAMES])> {
        if self.frames.len() < BLOCK_FRAMES {
            return None;
        }

        let first_frame_id = self.buffer_start_frame_id?;
        let mut block = [[0.0f32; 2]; BLOCK_FRAMES];
        for frame in &mut block {
            *frame = self.frames.pop_front().expect("length checked");
        }
        self.buffer_start_frame_id =
            Some(first_frame_id.saturating_add(BLOCK_FRAMES as u64));
        Some((first_frame_id, block))
    }
}

#[derive(Clone)]
struct LocalBlock {
    first_frame_id: u64,
    frames: [[f32; 2]; BLOCK_FRAMES],
}

#[derive(Default)]
struct LocalStats {
    rendered_frames: AtomicU64,
    underflow_frames: AtomicU64,
    queue_dropped_frames: AtomicU64,
    timeline_resets: AtomicU64,
    fifo_frames: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalStatsSnapshot {
    rendered_frames: u64,
    underflow_frames: u64,
    queue_dropped_frames: u64,
    timeline_resets: u64,
    fifo_frames: u64,
}

impl LocalStats {
    fn snapshot(&self) -> LocalStatsSnapshot {
        LocalStatsSnapshot {
            rendered_frames: self.rendered_frames.load(Ordering::Relaxed),
            underflow_frames: self.underflow_frames.load(Ordering::Relaxed),
            queue_dropped_frames: self
                .queue_dropped_frames
                .load(Ordering::Relaxed),
            timeline_resets: self.timeline_resets.load(Ordering::Relaxed),
            fifo_frames: self.fifo_frames.load(Ordering::Relaxed),
        }
    }
}

fn float_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

fn apply_gain(frames: &mut [[f32; 2]], gain: f32) {
    let gain = gain.clamp(0.0, 2.0);
    for frame in frames {
        frame[0] = (frame[0] * gain).clamp(-1.0, 1.0);
        frame[1] = (frame[1] * gain).clamp(-1.0, 1.0);
    }
}

fn push_local_block(
    sender: &SyncSender<LocalBlock>,
    block: LocalBlock,
    stats: &LocalStats,
) {
    match sender.try_send(block) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            stats.queue_dropped_frames.fetch_add(
                BLOCK_FRAMES as u64,
                Ordering::Relaxed,
            );
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn drain_source_queue(source: &ProcessLoopbackSource) -> Result<(), String> {
    loop {
        match source.recv_timeout(Duration::ZERO) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("process-loopback source queue disconnected".into())
            }
        }
    }
}

fn enqueue_local_block(
    source_fifo: &mut VecDeque<[f32; 2]>,
    delay: &mut AdjustableDelay,
    block: LocalBlock,
    expected_frame_id: &mut Option<u64>,
    stats: &LocalStats,
) {
    let gap = expected_frame_id
        .map(|expected| expected != block.first_frame_id)
        .unwrap_or(false);

    if gap {
        source_fifo.clear();
        delay.reset();
        stats.timeline_resets.fetch_add(1, Ordering::Relaxed);
    }

    source_fifo.extend(block.frames);
    *expected_frame_id =
        Some(block.first_frame_id.saturating_add(BLOCK_FRAMES as u64));
    stats
        .fifo_frames
        .store(source_fifo.len() as u64, Ordering::Relaxed);
}

fn pump_local_queue(
    receiver: &Receiver<LocalBlock>,
    source_fifo: &mut VecDeque<[f32; 2]>,
    delay: &mut AdjustableDelay,
    expected_frame_id: &mut Option<u64>,
    stats: &LocalStats,
) -> Result<(), &'static str> {
    loop {
        match receiver.try_recv() {
            Ok(block) => enqueue_local_block(
                source_fifo,
                delay,
                block,
                expected_frame_id,
                stats,
            ),
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err("local PCM queue disconnected")
            }
        }
    }
}

fn take_local_input(
    source_fifo: &mut VecDeque<[f32; 2]>,
    frames: usize,
    stats: &LocalStats,
) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        if let Some(frame) = source_fifo.pop_front() {
            out.push(frame);
        } else {
            out.push([0.0, 0.0]);
            stats.underflow_frames.fetch_add(1, Ordering::Relaxed);
        }
    }
    stats
        .fifo_frames
        .store(source_fifo.len() as u64, Ordering::Relaxed);
    out
}

fn run_local_render(
    device_substring: String,
    receiver: Receiver<LocalBlock>,
    controls: SharedControls,
    stats: Arc<LocalStats>,
) -> Result<(), String> {
    let sink = WasapiRenderSink::open(Some(&device_substring))
        .map_err(|error| format!("Realtek WASAPI open failed: {error}"))?;
    let initial_wasapi_frames = sink
        .available_frames()
        .map_err(|error| format!("Realtek available_frames failed: {error}"))?;

    println!(
        "[dual][local] device={} initial_wasapi={} startup_cushion={}f",
        sink.device_name,
        initial_wasapi_frames,
        LOCAL_STARTUP_CUSHION_FRAMES,
    );

    let mut source_fifo = VecDeque::<[f32; 2]>::new();
    let mut delay = AdjustableDelay::default();
    let mut expected_frame_id = None;

    let prime_target =
        initial_wasapi_frames.saturating_add(LOCAL_STARTUP_CUSHION_FRAMES);

    while !controls.stop_requested() && source_fifo.len() < prime_target {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(block) => enqueue_local_block(
                &mut source_fifo,
                &mut delay,
                block,
                &mut expected_frame_id,
                &stats,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("local PCM queue disconnected during prime".into())
            }
        }
    }

    if controls.stop_requested() {
        return Ok(());
    }

    if initial_wasapi_frames > 0 {
        let input = take_local_input(
            &mut source_fifo,
            initial_wasapi_frames,
            &stats,
        );
        let mut output =
            delay.process(&input, controls.local_delay_ms());
        apply_gain(&mut output, controls.local_gain());
        sink.write_float_stereo(&output)
            .map_err(|error| format!("Realtek initial write failed: {error}"))?;
        stats.rendered_frames.fetch_add(
            output.len() as u64,
            Ordering::Relaxed,
        );
    }

    sink.audio_client
        .start_stream()
        .map_err(|error| format!("Realtek start failed: {error}"))?;

    while !controls.stop_requested() {
        sink.event
            .wait_for_event(1_000)
            .map_err(|error| format!("Realtek event wait failed: {error}"))?;

        pump_local_queue(
            &receiver,
            &mut source_fifo,
            &mut delay,
            &mut expected_frame_id,
            &stats,
        )?;

        let available = sink
            .available_frames()
            .map_err(|error| format!("Realtek available_frames failed: {error}"))?;
        if available == 0 {
            continue;
        }

        let input =
            take_local_input(&mut source_fifo, available, &stats);
        let mut output =
            delay.process(&input, controls.local_delay_ms());
        apply_gain(&mut output, controls.local_gain());

        sink.write_float_stereo(&output)
            .map_err(|error| format!("Realtek write failed: {error}"))?;
        stats.rendered_frames.fetch_add(
            output.len() as u64,
            Ordering::Relaxed,
        );
    }

    let _ = sink.audio_client.stop_stream();
    Ok(())
}

fn run_dual_output(
    config: DualOutputGuiConfig,
    controls: SharedControls,
) -> Result<(), String> {
    let default_render = default_render_name()
        .map_err(|error| format!("cannot resolve default render: {error}"))?;

    if !default_render
        .to_lowercase()
        .contains(&config.sacrificial_substring.to_lowercase())
    {
        return Err(format!(
            "default render endpoint is not the requested sacrificial sink: expected substring {:?}, actual {:?}",
            config.sacrificial_substring,
            default_render,
        ));
    }

    println!(
        "[dual] sacrificial default render: {}",
        default_render
    );

    let socket = UdpSocket::bind("0.0.0.0:0")
        .map_err(|error| format!("UDP bind failed: {error}"))?;
    socket
        .connect(&config.target)
        .map_err(|error| format!("UDP connect failed: {error}"))?;

    let session_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock failed: {error}"))?
        .as_nanos() as u64
        ^ ((std::process::id() as u64) << 32);

    let codec = Lc3StreamConfig {
        sampling_frequency: SAMPLE_RATE_HZ,
        frame_duration: Lc3FrameDuration::TenMs,
        channels: 1,
        octets_per_codec_frame: LC3_OCTETS_PER_CHANNEL,
        codec_frames_per_sdu: 1,
    };
    let left_encoder = Lc3Encoder::new(codec)
        .map_err(|error| format!("left LC3 init failed: {error}"))?;
    let right_encoder = Lc3Encoder::new(codec)
        .map_err(|error| format!("right LC3 init failed: {error}"))?;

    let (local_tx, local_rx) =
        mpsc::sync_channel::<LocalBlock>(LOCAL_QUEUE_BLOCKS);
    let local_stats = Arc::new(LocalStats::default());
    let local_stats_thread = Arc::clone(&local_stats);
    let local_controls = controls.clone();
    let local_device = config.local_device_substring.clone();

    let local_join = thread::Builder::new()
        .name("s24-local-render".into())
        .spawn(move || {
            run_local_render(
                local_device,
                local_rx,
                local_controls,
                local_stats_thread,
            )
        })
        .map_err(|error| format!("cannot spawn local render: {error}"))?;

    let source = match ProcessLoopbackSource::start_excluding_current_process_tree(16) {
        Ok(source) => source,
        Err(error) => {
            controls.request_stop();
            let _ = local_join.join();
            return Err(format!(
                "Windows Process Loopback startup failed: {error:?}"
            ));
        }
    };

    println!(
        "[dual] target={} codec=LC3 48k/10ms/{}Bch local={} session={}",
        config.target,
        LC3_OCTETS_PER_CHANNEL,
        config.local_device_substring,
        session_id,
    );

    let mut assembler = FrameAssembler::default();
    let mut s24_delay = AdjustableDelay::default();
    let mut sequence = 0u64;
    let mut session_frame_id = 0u64;
    let mut packets_sent = 0u64;
    let mut bytes_sent = 0u64;
    let mut last_report = Instant::now();

    let stream_result: Result<(), String> = (|| {
        // Session-level warmup is part of the wire timeline. Both branches
        // receive the same integer number of 10 ms silent blocks so Android
        // AudioTrack, Samsung DSP, local WASAPI and both delay lines are hot
        // before any programme PCM is admitted.
        let silence = [[0.0f32; 2]; BLOCK_FRAMES];
        let block_period = Duration::from_millis(10);
        let mut next_deadline = Instant::now();

        for _ in 0..SESSION_PREROLL_PACKETS {
            if controls.stop_requested() {
                return Ok(());
            }

            // Process Loopback is already running so activation cost cannot
            // create a network hole. Discard all capture produced during
            // warmup; media epoch begins only after the final silent packet.
            drain_source_queue(&source)?;

            push_local_block(
                &local_tx,
                LocalBlock {
                    first_frame_id: session_frame_id,
                    frames: silence,
                },
                &local_stats,
            );

            let delayed =
                s24_delay.process(&silence, controls.s24_delay_ms());
            let mut left_pcm = [0i16; LC3_FRAME_SAMPLES];
            let mut right_pcm = [0i16; LC3_FRAME_SAMPLES];
            for (index, frame) in delayed.iter().enumerate() {
                left_pcm[index] = float_to_i16(frame[0]);
                right_pcm[index] = float_to_i16(frame[1]);
            }

            let left = left_encoder
                .encode_sdu(&left_pcm)
                .map_err(|error| format!("left LC3 preroll encode failed: {error}"))?;
            let right = right_encoder
                .encode_sdu(&right_pcm)
                .map_err(|error| format!("right LC3 preroll encode failed: {error}"))?;
            let bytes = encode_lc3_stereo(
                session_id,
                sequence,
                session_frame_id,
                &left,
                &right,
            )
            .map_err(|error| format!("UDP preroll encode failed: {error}"))?;

            let sent = socket
                .send(&bytes)
                .map_err(|error| format!("UDP preroll send failed: {error}"))?;
            if sent != bytes.len() {
                return Err(format!(
                    "partial UDP preroll send: {} of {} bytes",
                    sent,
                    bytes.len()
                ));
            }

            sequence = sequence.saturating_add(1);
            session_frame_id =
                session_frame_id.saturating_add(BLOCK_FRAMES as u64);
            packets_sent = packets_sent.saturating_add(1);
            bytes_sent = bytes_sent.saturating_add(sent as u64);

            next_deadline += block_period;
            let now = Instant::now();
            if next_deadline > now {
                thread::sleep(next_deadline - now);
            }
        }

        // Throw away anything captured during the last warmup interval. The
        // next fresh Process Loopback block defines programme epoch 0, mapped
        // onto the next aligned session packet boundary.
        drain_source_queue(&source)?;
        assembler = FrameAssembler::default();

        println!(
            "[dual][preroll] silence={}ms packets={} media_epoch_seq={} media_epoch_frame={}",
            SESSION_PREROLL_MS,
            SESSION_PREROLL_PACKETS,
            sequence,
            session_frame_id,
        );

        while !controls.stop_requested() {
            match source.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                assembler.push(
                    chunk.first_frame_id,
                    &chunk.frames,
                    chunk.discontinuity,
                );

                while let Some((_source_first_frame_id, pcm)) = assembler.pop_block() {
                    let first_frame_id = session_frame_id;
                    session_frame_id =
                        session_frame_id.saturating_add(BLOCK_FRAMES as u64);

                    push_local_block(
                        &local_tx,
                        LocalBlock {
                            first_frame_id,
                            frames: pcm,
                        },
                        &local_stats,
                    );

                    let delayed =
                        s24_delay.process(&pcm, controls.s24_delay_ms());
                    let mut left_pcm = [0i16; LC3_FRAME_SAMPLES];
                    let mut right_pcm = [0i16; LC3_FRAME_SAMPLES];

                    for (index, frame) in delayed.iter().enumerate() {
                        left_pcm[index] = float_to_i16(frame[0]);
                        right_pcm[index] = float_to_i16(frame[1]);
                    }

                    let left = left_encoder
                        .encode_sdu(&left_pcm)
                        .map_err(|error| format!("left LC3 encode failed: {error}"))?;
                    let right = right_encoder
                        .encode_sdu(&right_pcm)
                        .map_err(|error| format!("right LC3 encode failed: {error}"))?;

                    let bytes = encode_lc3_stereo(
                        session_id,
                        sequence,
                        first_frame_id,
                        &left,
                        &right,
                    )
                    .map_err(|error| format!("UDP frame encode failed: {error}"))?;

                    let sent = socket
                        .send(&bytes)
                        .map_err(|error| format!("UDP send failed: {error}"))?;
                    if sent != bytes.len() {
                        return Err(format!(
                            "partial UDP send: {} of {} bytes",
                            sent,
                            bytes.len()
                        ));
                    }

                    sequence = sequence.saturating_add(1);
                    packets_sent = packets_sent.saturating_add(1);
                    bytes_sent = bytes_sent.saturating_add(sent as u64);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("process-loopback source queue disconnected".into())
            }
        }

            if last_report.elapsed() >= Duration::from_secs(2) {
                let source_stats = source.stats();
                let local = local_stats.snapshot();
                println!(
                    "[dual] packets={} bytes={} captured={} src_drop={} disc={} asm_reset={} | local_fifo={} local_underflow={} local_qdrop={} local_reset={} | gain={:.2} s24_delay={}ms local_delay={}ms",
                    packets_sent,
                    bytes_sent,
                    source_stats.captured_frames,
                    source_stats.queue_dropped_frames,
                    source_stats.discontinuities,
                    assembler.resets,
                    local.fifo_frames,
                    local.underflow_frames,
                    local.queue_dropped_frames,
                    local.timeline_resets,
                    controls.local_gain(),
                    controls.s24_delay_ms(),
                    controls.local_delay_ms(),
                );
                last_report = Instant::now();
            }
        }
        Ok(())
    })();

    drop(local_tx);

    let source_result = source.shutdown().map_err(|error| {
        format!("Windows Process Loopback shutdown failed: {error:?}")
    });

    controls.request_stop();
    let local_result = local_join
        .join()
        .map_err(|_| "local render thread panicked".to_string())
        .and_then(|result| result);

    if let Err(error) = stream_result {
        let _ = source_result;
        let _ = local_result;
        return Err(error);
    }
    source_result?;
    local_result?;

    let local = local_stats.snapshot();
    println!(
        "[dual][done] packets={} bytes={} local_rendered={} local_underflow={} local_qdrop={} local_reset={}",
        packets_sent,
        bytes_sent,
        local.rendered_frames,
        local.underflow_frames,
        local.queue_dropped_frames,
        local.timeline_resets,
    );

    Ok(())
}

struct SessionWorker {
    controls: SharedControls,
    result_rx: Receiver<Result<(), String>>,
    join: Option<JoinHandle<()>>,
}

impl SessionWorker {
    fn start(
        config: DualOutputGuiConfig,
        local_volume_percent: f32,
        s24_delay_ms: u32,
        local_delay_ms: u32,
    ) -> Self {
        let controls = SharedControls::new();
        controls.set_local_gain(local_volume_percent / 100.0);
        controls.set_s24_delay_ms(s24_delay_ms);
        controls.set_local_delay_ms(local_delay_ms);
        let thread_controls = controls.clone();
        let (result_tx, result_rx) = mpsc::channel();

        let join = thread::Builder::new()
            .name("s24-dual-output-session".into())
            .spawn(move || {
                let result = run_dual_output(config, thread_controls);
                let _ = result_tx.send(result);
            })
            .expect("failed to spawn dual-output session");

        Self {
            controls,
            result_rx,
            join: Some(join),
        }
    }

    fn try_result(&self) -> Option<Result<(), String>> {
        match self.result_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "dual-output worker exited without a result".into(),
            )),
        }
    }

    fn stop(&self) {
        self.controls.request_stop();
    }

    fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct CalibrationWorker {
    result_rx: Receiver<Result<ChirpAlignment, String>>,
    join: Option<JoinHandle<()>>,
}

impl CalibrationWorker {
    fn start(config: &DualOutputGuiConfig) -> Self {
        let chirp_config = ChirpAlignConfig {
            target: config.target.clone(),
            local_device_substring: config.local_device_substring.clone(),
            mic_device_substring: config.mic_device_substring.clone(),
        };
        let (result_tx, result_rx) = mpsc::channel();

        let join = thread::Builder::new()
            .name("s24-gui-calibration".into())
            .spawn(move || {
                let result = measure_chirp_alignment(chirp_config)
                    .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
            })
            .expect("failed to spawn chirp calibration worker");

        Self {
            result_rx,
            join: Some(join),
        }
    }

    fn try_result(&self) -> Option<Result<ChirpAlignment, String>> {
        match self.result_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "calibration worker exited without a result".into(),
            )),
        }
    }

    fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct DualOutputApp {
    config: DualOutputGuiConfig,
    worker: Option<SessionWorker>,
    calibration: Option<CalibrationWorker>,
    local_volume_percent: f32,
    s24_delay_ms: u32,
    local_delay_ms: u32,
    last_calibration: Option<ChirpAlignment>,
    last_error: Option<String>,
}

impl DualOutputApp {
    fn new(config: DualOutputGuiConfig) -> Self {
        Self {
            config,
            worker: None,
            calibration: None,
            local_volume_percent: 100.0,
            s24_delay_ms: 0,
            local_delay_ms: 0,
            last_calibration: None,
            last_error: None,
        }
    }

    fn poll_session(&mut self) {
        let Some(result) = self
            .worker
            .as_ref()
            .and_then(SessionWorker::try_result)
        else {
            return;
        };

        let mut worker = self.worker.take().unwrap();
        worker.join();
        if let Err(error) = result {
            eprintln!("[dual][gui] session failed: {error}");
            self.last_error = Some(error);
        }
    }

    fn poll_calibration(&mut self) {
        let Some(result) = self
            .calibration
            .as_ref()
            .and_then(CalibrationWorker::try_result)
        else {
            return;
        };

        let mut calibration = self.calibration.take().unwrap();
        calibration.join();

        match result {
            Ok(alignment) => {
                self.local_delay_ms = alignment
                    .recommended_realtek_delay_ms
                    .round()
                    .clamp(0.0, MAX_DELAY_MS as f64)
                    as u32;
                self.s24_delay_ms = alignment
                    .recommended_s24_delay_ms
                    .round()
                    .clamp(0.0, MAX_DELAY_MS as f64)
                    as u32;
                self.last_calibration = Some(alignment);
                self.last_error = None;

                println!(
                    "[dual][calibrate] applied Realtek={}ms S24={}ms delta={:+.2}ms",
                    self.local_delay_ms,
                    self.s24_delay_ms,
                    alignment.delta_s24_minus_realtek_ms,
                );
            }
            Err(error) => {
                eprintln!("[dual][calibrate] failed: {error}");
                self.last_error = Some(error);
            }
        }
    }

    fn start_session(&mut self) {
        if self.worker.is_some() || self.calibration.is_some() {
            return;
        }

        self.last_error = None;
        self.worker = Some(SessionWorker::start(
            self.config.clone(),
            self.local_volume_percent,
            self.s24_delay_ms,
            self.local_delay_ms,
        ));
    }

    fn stop_session(&self) {
        if let Some(worker) = &self.worker {
            worker.stop();
        }
    }

    fn start_calibration(&mut self) {
        if self.worker.is_some() || self.calibration.is_some() {
            return;
        }

        self.last_error = None;
        self.calibration = Some(CalibrationWorker::start(&self.config));
    }
}

impl Drop for DualOutputApp {
    fn drop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
            worker.join();
        }
        if let Some(mut calibration) = self.calibration.take() {
            calibration.join();
        }
    }
}

impl eframe::App for DualOutputApp {
    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        _frame: &mut eframe::Frame,
    ) {
        self.poll_session();
        self.poll_calibration();

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.heading("S24MusicRelay · Dual output");
            ui.add_space(8.0);

            let running = self.worker.is_some();
            let calibrating = self.calibration.is_some();

            let volume = ui.add_enabled(
                !calibrating,
                egui::Slider::new(
                    &mut self.local_volume_percent,
                    0.0..=200.0,
                )
                .text("Realtek volume")
                .suffix("%"),
            );
            if volume.changed() {
                if let Some(worker) = &self.worker {
                    worker.controls.set_local_gain(
                        self.local_volume_percent / 100.0,
                    );
                }
            }

            let s24_delay = ui.add_enabled(
                !calibrating,
                egui::Slider::new(
                    &mut self.s24_delay_ms,
                    0..=MAX_DELAY_MS,
                )
                .text("S24 delay")
                .suffix(" ms"),
            );
            if s24_delay.changed() {
                if let Some(worker) = &self.worker {
                    worker.controls.set_s24_delay_ms(self.s24_delay_ms);
                }
            }

            let local_delay = ui.add_enabled(
                !calibrating,
                egui::Slider::new(
                    &mut self.local_delay_ms,
                    0..=MAX_DELAY_MS,
                )
                .text("Realtek delay")
                .suffix(" ms"),
            );
            if local_delay.changed() {
                if let Some(worker) = &self.worker {
                    worker.controls.set_local_delay_ms(self.local_delay_ms);
                }
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !running && !calibrating,
                        egui::Button::new("Start"),
                    )
                    .clicked()
                {
                    self.start_session();
                }

                if ui
                    .add_enabled(
                        running,
                        egui::Button::new("Stop"),
                    )
                    .clicked()
                {
                    self.stop_session();
                }

                if ui
                    .add_enabled(
                        !running && !calibrating,
                        egui::Button::new("Calibrate"),
                    )
                    .clicked()
                {
                    self.start_calibration();
                }
            });

            ui.add_space(8.0);
            ui.separator();

            if calibrating {
                ui.label(format!(
                    "Calibrating · Realtek/S24 alternating chirps ×5 · {}",
                    self.config.mic_device_substring,
                ));
            } else if running {
                ui.label("Running · stdout telemetry remains enabled");
            } else {
                ui.label("Idle");
            }

            if let Some(alignment) = self.last_calibration {
                ui.add_space(6.0);
                ui.monospace(format!(
                    "Calibration: Δ(S24-Realtek)={:+.2} ms · applied Realtek={} ms / S24={} ms · valid={}/{}",
                    alignment.delta_s24_minus_realtek_ms,
                    self.local_delay_ms,
                    self.s24_delay_ms,
                    alignment.valid_realtek_chirps,
                    alignment.valid_s24_chirps,
                ));
            }

            if let Some(error) = &self.last_error {
                ui.add_space(6.0);
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Error: {error}"),
                );
            }
        });

        ui.ctx().request_repaint_after(GUI_REPAINT_INTERVAL);
    }
}

pub fn run_dual_output_gui(
    config: DualOutputGuiConfig,
) -> Result<(), String> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 280.0])
            .with_min_inner_size([520.0, 240.0])
            .with_resizable(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "S24MusicRelay",
        native_options,
        Box::new(move |_cc| {
            Ok(Box::new(DualOutputApp::new(config.clone())))
        }),
    )
    .map_err(|error| format!("egui startup failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjustable_delay_zero_is_identity() {
        let mut delay = AdjustableDelay::default();
        let input = vec![[0.25, -0.25]; 480];
        assert_eq!(delay.process(&input, 0), input);
    }

    #[test]
    fn adjustable_delay_inserts_requested_silence() {
        let mut delay = AdjustableDelay::default();
        let first = vec![[1.0, 1.0]; 480];
        let second = vec![[2.0, 2.0]; 480];

        let out0 = delay.process(&first, 10);
        assert!(out0.iter().all(|frame| *frame == [0.0, 0.0]));

        let out1 = delay.process(&second, 10);
        assert_eq!(out1[0], [1.0, 1.0]);
        assert_eq!(out1[479], [1.0, 1.0]);
    }

    #[test]
    fn decreasing_delay_discards_history_instead_of_building_debt() {
        let mut delay = AdjustableDelay::default();
        let _ = delay.process(&vec![[1.0, 1.0]; 480], 10);
        let out = delay.process(&vec![[2.0, 2.0]; 480], 0);
        assert_eq!(out[0], [2.0, 2.0]);
    }
}
