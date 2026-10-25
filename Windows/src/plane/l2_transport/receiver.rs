//! UDP socket ownership.
//!
//! This module will own bind/receive and datagram admission only. Decoding,
//! reorder, concealment, and media-clock policy live elsewhere.

#[derive(Debug, Default)]
pub struct UdpReceiver;
