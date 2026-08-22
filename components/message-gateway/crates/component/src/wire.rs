use alloc::string::String;

use barracuda_event_router::rpc_message;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Wire capacity of a provider message identifier, including its terminator.
pub const MESSAGE_ID_CAPACITY: usize = 252;

/// Failure to encode or decode a Gateway wire value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GatewayWireError {
    /// A string exceeded the capacity of its RPC field.
    #[error("text exceeds the Gateway RPC field capacity")]
    TextTooLong,
    /// A string contained an embedded NUL byte.
    #[error("text contains a NUL byte")]
    EmbeddedNul,
    /// A wire string was not canonically NUL-terminated.
    #[error("text is not canonically NUL-terminated")]
    InvalidTerminator,
    /// A wire string was not valid UTF-8.
    #[error("text is not valid UTF-8")]
    InvalidUtf8,
    /// A binary chunk declared more bytes than it contains.
    #[error("binary chunk length exceeds its capacity")]
    InvalidChunk,
    /// A streamed request did not follow its RPC protocol.
    #[error("invalid Gateway request stream")]
    InvalidRequest,
}

/// Fixed-capacity, NUL-terminated UTF-8 text used by Gateway contracts.
///
/// The value holds at most `N - 1` UTF-8 bytes and serializes as a JSON
/// string when a contract is inspected through serde.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub struct GatewayText<const N: usize>([u8; N]);

impl<const N: usize> GatewayText<N> {
    /// Encodes one UTF-8 string into its fixed-capacity wire field.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayWireError::TextTooLong`] when the value does not leave
    /// space for a terminator, or [`GatewayWireError::EmbeddedNul`] when it
    /// contains a NUL byte.
    pub fn new(value: &str) -> Result<Self, GatewayWireError> {
        let bytes = value.as_bytes();
        if bytes.len() >= N {
            return Err(GatewayWireError::TextTooLong);
        }
        if bytes.contains(&0) {
            return Err(GatewayWireError::EmbeddedNul);
        }
        let mut buffer = [0_u8; N];
        buffer
            .get_mut(..bytes.len())
            .ok_or(GatewayWireError::TextTooLong)?
            .copy_from_slice(bytes);
        Ok(Self(buffer))
    }

    /// Decodes the canonical UTF-8 C string.
    ///
    /// # Errors
    ///
    /// Returns an error when the wire bytes are not canonical NUL-terminated
    /// UTF-8.
    pub fn as_str(&self) -> Result<&str, GatewayWireError> {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(GatewayWireError::InvalidTerminator)?;
        if self
            .0
            .get(end..)
            .ok_or(GatewayWireError::InvalidTerminator)?
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(GatewayWireError::InvalidTerminator);
        }
        core::str::from_utf8(
            self.0
                .get(..end)
                .ok_or(GatewayWireError::InvalidTerminator)?,
        )
        .map_err(|_error| GatewayWireError::InvalidUtf8)
    }
}

impl<const N: usize> Serialize for GatewayText<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de, const N: usize> Deserialize<'de> for GatewayText<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// Provider-assigned identifier returned by both Gateway send RPCs.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewaySendReceipt {
    message_id: GatewayText<MESSAGE_ID_CAPACITY>,
}

impl GatewaySendReceipt {
    /// Builds a receipt from a provider-assigned message identifier.
    ///
    /// # Errors
    ///
    /// Returns a wire error when the identifier cannot fit in the Gateway
    /// contract.
    pub fn new(message_id: &str) -> Result<Self, GatewayWireError> {
        Ok(Self {
            message_id: GatewayText::new(message_id)?,
        })
    }

    /// Returns the provider-assigned message identifier.
    ///
    /// # Errors
    ///
    /// Returns a wire error when bytes received from a lane are not canonical
    /// Gateway text.
    pub fn message_id(&self) -> Result<&str, GatewayWireError> {
        self.message_id.as_str()
    }
}
