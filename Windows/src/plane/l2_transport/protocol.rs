//! S24MusicRelay UDP v1 wire contract.
//!
//! All integer fields are little-endian. Transport sequence identity and media
//! source-frame identity are separate on purpose.

pub const MAGIC: [u8; 4] = *b"S24R";
pub const VERSION: u8 = 1;
pub const HEADER_BYTES: usize = 40;
pub const FORMAT_PCM16_LE: u8 = 1;
pub const SAMPLE_RATE_HZ: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const MAX_PACKET_FRAMES: usize = 240;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioPacket {
    pub session_id: u64,
    pub sequence: u64,
    pub first_frame_id: u64,
    pub frames: Vec<[f32; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketDecodeError {
    TooShort,
    Magic,
    Version,
    HeaderSize,
    Format,
    FrameCount,
    Length,
}


pub fn encode_pcm16(
    session_id: u64,
    sequence: u64,
    first_frame_id: u64,
    frames: &[[i16; 2]],
) -> Result<Vec<u8>, PacketDecodeError> {
    if frames.is_empty() || frames.len() > MAX_PACKET_FRAMES {
        return Err(PacketDecodeError::FrameCount);
    }

    let mut bytes = vec![0u8; HEADER_BYTES + frames.len() * CHANNELS as usize * 2];
    bytes[0..4].copy_from_slice(&MAGIC);
    bytes[4] = VERSION;
    bytes[5] = 0;
    bytes[6..8].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
    bytes[8..16].copy_from_slice(&session_id.to_le_bytes());
    bytes[16..24].copy_from_slice(&sequence.to_le_bytes());
    bytes[24..32].copy_from_slice(&first_frame_id.to_le_bytes());
    bytes[32..34].copy_from_slice(&(frames.len() as u16).to_le_bytes());
    bytes[34] = CHANNELS;
    bytes[35] = FORMAT_PCM16_LE;
    bytes[36..40].copy_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());

    let mut offset = HEADER_BYTES;
    for frame in frames {
        bytes[offset..offset + 2].copy_from_slice(&frame[0].to_le_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&frame[1].to_le_bytes());
        offset += 4;
    }

    Ok(bytes)
}

pub fn decode(datagram: &[u8]) -> Result<AudioPacket, PacketDecodeError> {
    if datagram.len() < HEADER_BYTES {
        return Err(PacketDecodeError::TooShort);
    }
    if datagram[0..4] != MAGIC {
        return Err(PacketDecodeError::Magic);
    }
    if datagram[4] != VERSION {
        return Err(PacketDecodeError::Version);
    }

    let header_size =
        u16::from_le_bytes([datagram[6], datagram[7]]) as usize;
    if header_size != HEADER_BYTES {
        return Err(PacketDecodeError::HeaderSize);
    }

    let session_id =
        u64::from_le_bytes(datagram[8..16].try_into().unwrap());
    let sequence =
        u64::from_le_bytes(datagram[16..24].try_into().unwrap());
    let first_frame_id =
        u64::from_le_bytes(datagram[24..32].try_into().unwrap());
    let frame_count =
        u16::from_le_bytes(datagram[32..34].try_into().unwrap()) as usize;
    let channels = datagram[34];
    let format = datagram[35];
    let sample_rate =
        u32::from_le_bytes(datagram[36..40].try_into().unwrap());

    if channels != CHANNELS
        || format != FORMAT_PCM16_LE
        || sample_rate != SAMPLE_RATE_HZ
    {
        return Err(PacketDecodeError::Format);
    }
    if frame_count == 0 || frame_count > MAX_PACKET_FRAMES {
        return Err(PacketDecodeError::FrameCount);
    }

    let expected =
        HEADER_BYTES + frame_count * CHANNELS as usize * 2;
    if datagram.len() != expected {
        return Err(PacketDecodeError::Length);
    }

    let mut frames = Vec::with_capacity(frame_count);
    let mut offset = HEADER_BYTES;

    for _ in 0..frame_count {
        let left =
            i16::from_le_bytes([datagram[offset], datagram[offset + 1]]);
        let right =
            i16::from_le_bytes([datagram[offset + 2], datagram[offset + 3]]);
        offset += 4;

        frames.push([
            left as f32 / 32_768.0,
            right as f32 / 32_768.0,
        ]);
    }

    Ok(AudioPacket {
        session_id,
        sequence,
        first_frame_id,
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn encode_round_trips_pcm16_v1() {
        let source = [[16_384i16, -16_384i16], [0i16, 32_767i16]];
        let bytes = encode_pcm16(7, 9, 240, &source).unwrap();
        let packet = decode(&bytes).unwrap();

        assert_eq!(packet.session_id, 7);
        assert_eq!(packet.sequence, 9);
        assert_eq!(packet.first_frame_id, 240);
        assert_eq!(packet.frames.len(), 2);
        assert!((packet.frames[0][0] - 0.5).abs() < 1.0e-6);
        assert!((packet.frames[0][1] + 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn decodes_known_v1_packet() {
        let mut bytes = vec![0u8; HEADER_BYTES + 8];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4] = VERSION;
        bytes[6..8].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        bytes[8..16].copy_from_slice(&7u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&9u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&240u64.to_le_bytes());
        bytes[32..34].copy_from_slice(&2u16.to_le_bytes());
        bytes[34] = CHANNELS;
        bytes[35] = FORMAT_PCM16_LE;
        bytes[36..40].copy_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());

        bytes[40..42].copy_from_slice(&16_384i16.to_le_bytes());
        bytes[42..44].copy_from_slice(&(-16_384i16).to_le_bytes());
        bytes[44..46].copy_from_slice(&0i16.to_le_bytes());
        bytes[46..48].copy_from_slice(&32_767i16.to_le_bytes());

        let packet = decode(&bytes).unwrap();
        assert_eq!(packet.session_id, 7);
        assert_eq!(packet.sequence, 9);
        assert_eq!(packet.first_frame_id, 240);
        assert_eq!(packet.frames.len(), 2);
        assert!((packet.frames[0][0] - 0.5).abs() < 1.0e-6);
        assert!((packet.frames[0][1] + 0.5).abs() < 1.0e-6);
    }
}
