//! Event-driven shared-mode WASAPI render adapter.
//!
//! WASAPI is the output pacing authority. The callback asks Media for exactly
//! the number of stream frames Windows has made available.

#[cfg(target_os = "windows")]
use wasapi::{
    Device, DeviceEnumerator, Direction, SampleType, ShareMode, StreamMode,
    WaveFormat, initialize_mta,
};

#[cfg(target_os = "windows")]
pub struct WasapiRenderSink {
    pub device_name: String,
    pub audio_client: wasapi::AudioClient,
    pub render_client: wasapi::AudioRenderClient,
    pub event: wasapi::Handle,
    pub block_align: usize,
}

#[cfg(target_os = "windows")]
impl WasapiRenderSink {
    pub fn open(
        device_substring: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        initialize_mta().ok()?;

        let enumerator = DeviceEnumerator::new()?;
        let device = select_device(&enumerator, device_substring)?;
        let device_name = device.get_friendlyname()?;
        let mut audio_client = device.get_iaudioclient()?;

        let desired =
            WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
        let block_align = desired.get_blockalign() as usize;

        let exact = matches!(
            audio_client.is_supported(&desired, &ShareMode::Shared),
            Ok(None)
        );
        let mix = audio_client.get_mixformat()?;
        let (default_period, minimum_period) =
            audio_client.get_device_period()?;

        println!("[wasapi] device: {device_name}");
        println!("[wasapi] requested: 48 kHz Float32 stereo");
        println!("[wasapi] mix format: {mix:?}");
        println!(
            "[wasapi] shared exact={} default_period={} min_period={}",
            exact, default_period, minimum_period
        );

        let mode = StreamMode::EventsShared {
            autoconvert: !exact,
            buffer_duration_hns: default_period,
        };
        audio_client.initialize_client(
            &desired,
            &Direction::Render,
            &mode,
        )?;

        let event = audio_client.set_get_eventhandle()?;
        let render_client = audio_client.get_audiorenderclient()?;

        Ok(Self {
            device_name,
            audio_client,
            render_client,
            event,
            block_align,
        })
    }

    pub fn available_frames(&self) -> Result<usize, wasapi::WasapiError> {
        Ok(self.audio_client.get_available_space_in_frames()? as usize)
    }

    pub fn write_float_stereo(
        &self,
        frames: &[[f32; 2]],
    ) -> Result<(), wasapi::WasapiError> {
        let mut bytes = vec![0u8; frames.len() * self.block_align];
        for (frame, dst) in frames
            .iter()
            .zip(bytes.chunks_exact_mut(self.block_align))
        {
            dst[0..4].copy_from_slice(&frame[0].to_le_bytes());
            dst[4..8].copy_from_slice(&frame[1].to_le_bytes());
        }

        self.render_client
            .write_to_device(frames.len(), &bytes, None)
    }
}

#[cfg(target_os = "windows")]
fn select_device(
    enumerator: &DeviceEnumerator,
    substring: Option<&str>,
) -> Result<Device, Box<dyn std::error::Error>> {
    let Some(needle) = substring else {
        return Ok(enumerator.get_default_device(&Direction::Render)?);
    };

    let needle = needle.to_lowercase();
    let collection =
        enumerator.get_device_collection(&Direction::Render)?;

    for entry in &collection {
        let device = entry?;
        let name = device.get_friendlyname()?;
        if name.to_lowercase().contains(&needle) {
            return Ok(device);
        }
    }

    Err(format!("no render device matched {needle:?}").into())
}
