//! Event-driven shared-mode microphone capture for acoustic calibration.

#[cfg(target_os = "windows")]
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

#[cfg(target_os = "windows")]
use wasapi::{
    Device, DeviceEnumerator, Direction, SampleType, ShareMode, StreamMode,
    WaveFormat, initialize_mta,
};

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct MicCaptureResult {
    pub device_name: String,
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

#[cfg(target_os = "windows")]
pub struct MicCapture {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<MicCaptureResult, String>>>,
}

#[cfg(target_os = "windows")]
impl MicCapture {
    pub fn start(device_substring: &str) -> Result<Self, String> {
        let needle = device_substring.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);

        let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);

        let join = thread::Builder::new()
            .name("s24-mic-capture".into())
            .spawn(move || {
                let result = run_capture(&needle, thread_stop, &startup_tx);
                result
            })
            .map_err(|error| format!("cannot spawn mic capture: {error}"))?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err("mic capture startup channel closed".into())
            }
        }
    }

    pub fn stop(mut self) -> Result<MicCaptureResult, String> {
        self.stop.store(true, Ordering::Release);
        self.join
            .take()
            .ok_or_else(|| "mic capture thread missing".to_string())?
            .join()
            .map_err(|_| "mic capture thread panicked".to_string())?
    }
}

#[cfg(target_os = "windows")]
impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(target_os = "windows")]
fn select_capture_device(
    enumerator: &DeviceEnumerator,
    substring: &str,
) -> Result<Device, String> {
    let needle = substring.to_lowercase();
    let collection = enumerator
        .get_device_collection(&Direction::Capture)
        .map_err(|error| format!("enumerate capture endpoints failed: {error}"))?;

    let mut matches = Vec::new();
    let mut names = Vec::new();

    for entry in &collection {
        let device = entry
            .map_err(|error| format!("capture endpoint enumeration failed: {error}"))?;
        let name = device
            .get_friendlyname()
            .map_err(|error| format!("capture endpoint name failed: {error}"))?;
        names.push(name.clone());

        if name.to_lowercase().contains(&needle) {
            matches.push((device, name));
        }
    }

    if matches.len() != 1 {
        return Err(format!(
            "expected exactly one capture endpoint matching {:?}, found {}; available={:?}",
            substring,
            matches.len(),
            names,
        ));
    }

    Ok(matches.remove(0).0)
}

#[cfg(target_os = "windows")]
fn run_capture(
    device_substring: &str,
    stop: Arc<AtomicBool>,
    startup: &std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<MicCaptureResult, String> {
    initialize_mta()
        .ok()
        .map_err(|error| format!("mic COM init failed: {error}"))?;

    let enumerator = DeviceEnumerator::new()
        .map_err(|error| format!("mic enumerator failed: {error}"))?;
    let device = select_capture_device(&enumerator, device_substring)?;
    let device_name = device
        .get_friendlyname()
        .map_err(|error| format!("mic endpoint name failed: {error}"))?;

    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|error| format!("mic AudioClient failed: {error}"))?;

    let desired =
        WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    let block_align = desired.get_blockalign() as usize;

    let (_, minimum_period) = audio_client
        .get_device_period()
        .map_err(|error| format!("mic device period failed: {error}"))?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: minimum_period,
    };

    audio_client
        .initialize_client(&desired, &Direction::Capture, &mode)
        .map_err(|error| format!("mic initialize failed: {error}"))?;

    let event = audio_client
        .set_get_eventhandle()
        .map_err(|error| format!("mic event handle failed: {error}"))?;
    let capture = audio_client
        .get_audiocaptureclient()
        .map_err(|error| format!("mic capture client failed: {error}"))?;

    let mut bytes = VecDeque::<u8>::new();
    let mut samples = Vec::<f32>::with_capacity(800_000);

    audio_client
        .start_stream()
        .map_err(|error| format!("mic start failed: {error}"))?;

    println!(
        "[chirp][mic] device={} requested=48k Float32 stereo",
        device_name
    );

    let _ = startup.send(Ok(()));

    while !stop.load(Ordering::Acquire) {
        if event.wait_for_event(100).is_err() {
            continue;
        }

        capture
            .read_from_device_to_deque(&mut bytes)
            .map_err(|error| format!("mic read failed: {error}"))?;

        let complete_bytes = bytes.len() - (bytes.len() % block_align);
        let frames = complete_bytes / block_align;

        for _ in 0..frames {
            let mut raw = [0u8; 4];
            for byte in &mut raw {
                *byte = bytes.pop_front().unwrap_or(0);
            }
            samples.push(f32::from_le_bytes(raw));

            for _ in 4..block_align {
                let _ = bytes.pop_front();
            }
        }
    }

    let _ = audio_client.stop_stream();

    Ok(MicCaptureResult {
        device_name,
        samples,
        sample_rate_hz: 48_000,
    })
}

#[cfg(not(target_os = "windows"))]
pub struct MicCapture;

#[cfg(not(target_os = "windows"))]
pub struct MicCaptureResult;

#[cfg(not(target_os = "windows"))]
impl MicCapture {
    pub fn start(_device_substring: &str) -> Result<Self, String> {
        Err("microphone capture is only available on Windows".into())
    }
}
