//! Framing for the private virtual-NIC transport between a Platform and the gateway.
//!
//! Every WebSocket binary message contains exactly one frame. Keeping the framing
//! independent of WebSocket also permits the macOS Platform to use a Unix socket.

#![no_std]

/// Current protocol version, negotiated in the initial hello frame.
pub const VERSION: u8 = 1;
/// Maximum IPv4 packet accepted by the gateway.
pub const MAX_PACKET_SIZE: usize = 65_535;
const HEADER_SIZE: usize = 2;

/// Message kinds carried by the virtual-NIC transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    /// Client greeting; payload is the protocol version byte.
    Hello = 1,
    /// Raw layer-three IP packet.
    Packet = 2,
    /// UTF-8 public URL assigned to the device's web server.
    DeviceUrl = 3,
    /// UTF-8 diagnostic followed by connection shutdown.
    Error = 4,
}

impl TryFrom<u8> for Kind {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, DecodeError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Packet),
            3 => Ok(Self::DeviceUrl),
            4 => Ok(Self::Error),
            value => Err(DecodeError::UnknownKind(value)),
        }
    }
}

/// A borrowed decoded transport frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame<'a> {
    /// Frame discriminator.
    pub kind: Kind,
    /// Frame contents.
    pub payload: &'a [u8],
}

/// Returns the encoded size for a payload.
#[must_use]
pub const fn encoded_len(payload_len: usize) -> usize {
    HEADER_SIZE + payload_len
}

/// Encodes one frame into a caller-owned buffer.
pub fn encode(kind: Kind, payload: &[u8], output: &mut [u8]) -> Result<usize, EncodeError> {
    if payload.len() > MAX_PACKET_SIZE {
        return Err(EncodeError::PayloadTooLarge);
    }
    let needed = encoded_len(payload.len());
    if output.len() < needed {
        return Err(EncodeError::BufferTooSmall);
    }
    output[0] = VERSION;
    output[1] = kind as u8;
    output[HEADER_SIZE..needed].copy_from_slice(payload);
    Ok(needed)
}

/// Decodes one complete WebSocket binary message.
pub fn decode(input: &[u8]) -> Result<Frame<'_>, DecodeError> {
    if input.len() < HEADER_SIZE {
        return Err(DecodeError::Truncated);
    }
    if input[0] != VERSION {
        return Err(DecodeError::UnsupportedVersion(input[0]));
    }
    let kind = input[1].try_into()?;
    let payload = &input[HEADER_SIZE..];
    if payload.len() > MAX_PACKET_SIZE {
        return Err(DecodeError::PayloadTooLarge);
    }
    if kind == Kind::Hello && payload != [VERSION] {
        return Err(DecodeError::InvalidHello);
    }
    Ok(Frame { kind, payload })
}

/// Encoding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Destination cannot hold the complete frame.
    BufferTooSmall,
    /// Payload exceeds the largest IP packet.
    PayloadTooLarge,
}

/// Decoding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Message does not contain the two-byte header.
    Truncated,
    /// Peer uses an incompatible protocol version.
    UnsupportedVersion(u8),
    /// Message kind is not defined by this version.
    UnknownKind(u8),
    /// Payload exceeds the largest IP packet.
    PayloadTooLarge,
    /// Hello payload did not repeat the negotiated version.
    InvalidHello,
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, encoded_len, DecodeError, Kind, VERSION};

    #[test]
    fn packet_round_trips_without_allocating() {
        let packet = [0x45, 0, 0, 20];
        let mut encoded = [0; 6];
        let length = encode(Kind::Packet, &packet, &mut encoded);
        assert_eq!(length, Ok(encoded_len(packet.len())));
        let length = length.unwrap_or_default();
        assert_eq!(length, encoded_len(packet.len()));
        assert_eq!(
            decode(&encoded).map(|frame| frame.payload),
            Ok(packet.as_slice())
        );
    }

    #[test]
    fn incompatible_peer_is_rejected() {
        assert_eq!(
            decode(&[VERSION + 1, Kind::Hello as u8, VERSION]),
            Err(DecodeError::UnsupportedVersion(VERSION + 1))
        );
    }
}
