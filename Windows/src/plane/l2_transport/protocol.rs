//! Wire-format contract between Android sender and Windows receiver.
//!
//! Header fields and ABI are intentionally not frozen yet. The contract will
//! carry transport sequence identity separately from source-frame identity.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketSequence(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrameId(pub u64);
