use core::fmt;

use barracuda_event_router::{JsonPayload, RpcError};

pub(crate) const FRAME_CAPACITY: usize = 512;

pub(crate) fn event_input_capacity<const M: usize>(event_id: &str) -> Result<usize, RpcError> {
    const ENVELOPE_WITHOUT_EVENT_ID: usize = r#"{"event":"","input":}"#.len();
    let envelope = ENVELOPE_WITHOUT_EVENT_ID
        .checked_add(event_id.len())
        .ok_or(RpcError::InvalidFrameState)?;
    M.checked_sub(envelope).ok_or(RpcError::FrameTooLarge {
        size: envelope,
        capacity: M,
    })
}

pub(crate) trait EncodedJson {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result;
}

pub(crate) fn encoded_json_len(value: &impl EncodedJson) -> Result<usize, RpcError> {
    let mut writer = LengthWriter::default();
    value
        .encode(&mut writer)
        .map_err(|_error| RpcError::InvalidFrameState)?;
    Ok(writer.written)
}

pub(crate) fn write_encoded_json(
    value: &impl EncodedJson,
    destination: &mut [u8],
) -> Result<usize, RpcError> {
    let size = encoded_json_len(value)?;
    let capacity = destination.len();
    let mut writer = SliceWriter::new(destination);
    value
        .encode(&mut writer)
        .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
    if writer.written != size {
        return Err(RpcError::InvalidFrameState);
    }
    Ok(size)
}

pub(crate) fn write_json_string(writer: &mut impl fmt::Write, value: &str) -> fmt::Result {
    writer.write_char('"')?;
    for character in value.chars() {
        match character {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            control @ '\u{0000}'..='\u{001f}' => {
                write!(writer, "\\u{:04x}", u32::from(control))?;
            }
            other => writer.write_char(other)?,
        }
    }
    writer.write_char('"')
}

pub(crate) fn bounded_json_prefix(value: &str, max_encoded_bytes: usize) -> &str {
    let mut encoded = 0_usize;
    let mut end = 0_usize;
    for (index, character) in value.char_indices() {
        let bytes = match character {
            '"' | '\\' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        let Some(next) = encoded.checked_add(bytes) else {
            break;
        };
        if next > max_encoded_bytes {
            break;
        }
        encoded = next;
        end = index.saturating_add(character.len_utf8());
    }
    value.get(..end).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GatewayJsonError {
    InvalidRequest,
    UnknownChannel,
    DuplicateStream,
    UnknownStream,
    OutOfOrder,
    Busy,
    Unsupported,
    Authentication,
    RateLimited,
    Delivery,
    InvalidReceipt,
}

impl GatewayJsonError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UnknownChannel => "unknown_channel",
            Self::DuplicateStream => "duplicate_stream",
            Self::UnknownStream => "unknown_stream",
            Self::OutOfOrder => "out_of_order",
            Self::Busy => "busy",
            Self::Unsupported => "unsupported",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::Delivery => "delivery",
            Self::InvalidReceipt => "invalid_receipt",
        }
    }
}

pub(crate) struct ErrorResponse(pub(crate) GatewayJsonError);

impl EncodedJson for ErrorResponse {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        write!(writer, "{{\"error\":\"{}\"}}", self.0.code())
    }
}

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

pub(crate) struct AckResponse {
    pub(crate) accepted_sequence: u32,
}

impl EncodedJson for AckResponse {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        write!(
            writer,
            "{{\"accepted_sequence\":{}}}",
            self.accepted_sequence
        )
    }
}

impl JsonPayload for AckResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

pub(crate) const fn valid_required(value: &str) -> bool {
    !value.is_empty()
}

pub(crate) fn valid_stream_id(value: &str) -> bool {
    valid_required(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[derive(Default)]
struct LengthWriter {
    written: usize,
}

impl fmt::Write for LengthWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.written = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl<'a> SliceWriter<'a> {
    const fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            written: 0,
        }
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let output = self
            .destination
            .get_mut(self.written..end)
            .ok_or(fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
