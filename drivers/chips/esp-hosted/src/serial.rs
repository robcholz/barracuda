//! The serial interface that carries RPC messages.
//!
//! Each RPC message travels as two TLVs (`compose_tlv`/`parse_tlv` in
//! ESP-Hosted-MCU `host/drivers/virtual_serial_if/serial_if.c`): the endpoint
//! name, then the protobuf bytes, each with a one-byte type and a 16-bit
//! little-endian length. Requests go to the `RPCRsp` endpoint; responses come
//! back on `RPCRsp` and events on `RPCEvt`. A message longer than one packet
//! arrives in fragments that set the header's more-fragments flag on every
//! packet but the last.

use alloc::vec::Vec;
use core::fmt;

/// The endpoint requests and responses use.
pub const ENDPOINT_RESPONSE: &[u8] = b"RPCRsp";
/// The endpoint events use.
pub const ENDPOINT_EVENT: &[u8] = b"RPCEvt";

const TYPE_ENDPOINT: u8 = 0x01;
const TYPE_DATA: u8 = 0x02;

/// Largest reassembled RPC message accepted from the co-processor; a scan
/// listing of the 20 records this host asks for stays far below it.
pub const MAX_MESSAGE: usize = 8 * 1024;

/// Which endpoint a message arrived on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// `RPCRsp`: a response to a request.
    Response,
    /// `RPCEvt`: an unsolicited event.
    Event,
}

/// Wraps an encoded RPC request for the serial interface.
#[must_use]
pub fn compose(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + ENDPOINT_RESPONSE.len() + 6);
    out.push(TYPE_ENDPOINT);
    out.extend_from_slice(&(ENDPOINT_RESPONSE.len() as u16).to_le_bytes());
    out.extend_from_slice(ENDPOINT_RESPONSE);
    out.push(TYPE_DATA);
    out.extend_from_slice(&(message.len() as u16).to_le_bytes());
    out.extend_from_slice(message);
    out
}

/// Splits a reassembled serial message into its endpoint and protobuf bytes.
///
/// # Errors
///
/// Returns an error for a message that is not an endpoint TLV followed by a
/// data TLV, or whose endpoint is neither `RPCRsp` nor `RPCEvt`.
pub fn parse(bytes: &[u8]) -> Result<(Endpoint, &[u8]), SerialError> {
    let (endpoint, rest) = tlv(bytes, TYPE_ENDPOINT)?;
    let endpoint = match endpoint {
        ENDPOINT_RESPONSE => Endpoint::Response,
        ENDPOINT_EVENT => Endpoint::Event,
        _ => return Err(SerialError::UnknownEndpoint),
    };
    let (data, _) = tlv(rest, TYPE_DATA)?;
    Ok((endpoint, data))
}

fn tlv(bytes: &[u8], expected: u8) -> Result<(&[u8], &[u8]), SerialError> {
    let [kind, low, high, rest @ ..] = bytes else {
        return Err(SerialError::Truncated);
    };
    if *kind != expected {
        return Err(SerialError::UnexpectedType(*kind));
    }
    let length = usize::from(u16::from_le_bytes([*low, *high]));
    if rest.len() < length {
        return Err(SerialError::Truncated);
    }
    Ok(rest.split_at(length))
}

/// Joins the fragments of one serial message.
#[derive(Debug, Default)]
pub struct Reassembler {
    message: Vec<u8>,
    discarding: bool,
}

impl Reassembler {
    /// Creates an empty reassembler.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            message: Vec::new(),
            discarding: false,
        }
    }

    /// Adds one packet's payload and returns the message once its last
    /// fragment arrived.
    ///
    /// # Errors
    ///
    /// Returns [`SerialError::TooLarge`] once for a message larger than
    /// [`MAX_MESSAGE`]; its remaining fragments are dropped.
    pub fn push(&mut self, payload: &[u8], more: bool) -> Result<Option<Vec<u8>>, SerialError> {
        if self.discarding {
            if !more {
                self.discarding = false;
            }
            return Ok(None);
        }
        if self.message.len() + payload.len() > MAX_MESSAGE {
            self.message.clear();
            self.discarding = more;
            return Err(SerialError::TooLarge);
        }
        self.message.extend_from_slice(payload);
        if more {
            Ok(None)
        } else {
            Ok(Some(core::mem::take(&mut self.message)))
        }
    }
}

/// A serial message is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialError {
    /// A TLV ends early.
    Truncated,
    /// A TLV has an unexpected type.
    UnexpectedType(u8),
    /// The endpoint is neither `RPCRsp` nor `RPCEvt`.
    UnknownEndpoint,
    /// The message exceeds [`MAX_MESSAGE`].
    TooLarge,
}

impl fmt::Display for SerialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("RPC message is truncated"),
            Self::UnexpectedType(kind) => {
                write!(formatter, "RPC TLV type {kind:#04x} is unexpected")
            }
            Self::UnknownEndpoint => formatter.write_str("RPC endpoint is unknown"),
            Self::TooLarge => formatter.write_str("RPC message is too large"),
        }
    }
}

impl core::error::Error for SerialError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_request_is_framed_as_the_rpcrsp_endpoint_then_its_data() {
        assert_eq!(
            compose(&[0x08, 0x01]),
            [
                0x01, 0x06, 0x00, b'R', b'P', b'C', b'R', b's', b'p', 0x02, 0x02, 0x00, 0x08, 0x01
            ]
        );
    }

    #[test]
    fn responses_and_events_are_told_apart_by_endpoint() {
        let response = compose(&[1, 2, 3]);
        assert_eq!(
            parse(&response).unwrap(),
            (Endpoint::Response, &[1, 2, 3][..])
        );

        let mut event = response.clone();
        event[3..9].copy_from_slice(b"RPCEvt");
        assert_eq!(parse(&event).unwrap(), (Endpoint::Event, &[1, 2, 3][..]));

        event[3..9].copy_from_slice(b"ctrlRs");
        assert_eq!(parse(&event), Err(SerialError::UnknownEndpoint));
        assert_eq!(parse(&response[..12]), Err(SerialError::Truncated));
    }

    #[test]
    fn fragments_join_until_the_last_one() {
        let mut reassembler = Reassembler::new();
        assert_eq!(reassembler.push(&[1, 2], true).unwrap(), None);
        assert_eq!(reassembler.push(&[3], true).unwrap(), None);
        assert_eq!(
            reassembler.push(&[4], false).unwrap(),
            Some(alloc::vec![1, 2, 3, 4])
        );
        assert_eq!(reassembler.push(&[5], false).unwrap(), Some(alloc::vec![5]));
    }

    #[test]
    fn an_oversized_message_is_dropped_whole() {
        let mut reassembler = Reassembler::new();
        let chunk = alloc::vec![0; 1500];
        for _ in 0..5 {
            assert_eq!(reassembler.push(&chunk, true).unwrap(), None);
        }
        assert_eq!(reassembler.push(&chunk, true), Err(SerialError::TooLarge));
        assert_eq!(reassembler.push(&chunk, false).unwrap(), None);
        assert_eq!(reassembler.push(&[7], false).unwrap(), Some(alloc::vec![7]));
    }
}
