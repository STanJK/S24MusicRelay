//! Session and canonical media-format contracts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalAudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl CanonicalAudioFormat {
    pub const S24_PCM16_STEREO_48K: Self = Self {
        sample_rate_hz: 48_000,
        channels: 2,
    };
}
