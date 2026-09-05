//! Public JSON Event declaration and emission API.

use alloc::string::String;
use core::fmt;

use barracuda_rpc::{
    JsonObjectFields, JsonObjectPayload, JsonObjectWriter, JsonPayload, JsonRpcSchema, RpcAddress,
    RpcClient, RpcError,
};

use super::ingress::InternalEmit;
use super::Topic;

/// A validated identifier for one Event type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(String);

impl EventId {
    /// Returns this Event ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for EventId {
    type Error = EventIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(String::from(value))
    }
}

impl TryFrom<String> for EventId {
    type Error = EventIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(EventIdError::Empty);
        }
        for (index, character) in value.char_indices() {
            let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.');
            if !valid {
                return Err(EventIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }
}

impl AsRef<str> for EventId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while parsing an [`EventId`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EventIdError {
    /// Event IDs cannot be empty.
    #[error("Event ID cannot be empty")]
    Empty,
    /// Event IDs contain only ASCII letters, digits, `_`, `-`, and `.`.
    #[error("invalid Event ID character {character:?} at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
        /// Invalid character found in the input.
        character: char,
    },
}

/// Compile-time identity of one JSON Event.
pub trait Event: 'static {
    /// Stable Event identifier used by Workflow matching.
    const ID: &'static str;
}

/// Failure returned by [`EventEmitter::emit`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmitError {
    /// The Event type declares an invalid Event ID.
    #[error("invalid Event ID: {0}")]
    InvalidEventId(#[from] EventIdError),
    /// RPC transport, JSON encoding, or runtime failure.
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// JSON Event producer backed by a Component's RPC client.
#[derive(Clone)]
pub struct EventEmitter<const M: usize> {
    rpc: RpcClient,
}

impl<const M: usize> EventEmitter<M> {
    /// Wraps an existing client without creating another registry or transport.
    #[must_use]
    pub const fn new(rpc: RpcClient) -> Self {
        Self { rpc }
    }

    /// Emits one JSON Event and waits until Workflow Runtime accepts ownership.
    ///
    /// This does not wait for matching Workflow executions or downstream RPCs
    /// to complete. The input is encoded directly into the internal request
    /// lane through [`JsonPayload`].
    pub async fn emit<E>(&self, input: &(impl JsonPayload + ?Sized)) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(None, input).await
    }

    /// Emits one JSON Event with a topic used by optional Workflow filtering.
    pub async fn emit_to<E>(
        &self,
        topic: &Topic,
        input: &(impl JsonPayload + ?Sized),
    ) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(Some(topic), input).await
    }

    async fn emit_inner<E>(
        &self,
        topic: Option<&Topic>,
        input: &(impl JsonPayload + ?Sized),
    ) -> Result<(), EmitError>
    where
        E: Event,
    {
        let event = EventId::try_from(E::ID)?;
        let fields = EmitFields {
            event: event.as_str(),
            topic,
            input,
        };
        let document = JsonObjectPayload::new(&fields);
        let address = RpcAddress::try_from(InternalEmit::<M>::ADDRESS)
            .map_err(|_error| EmitError::Rpc(RpcError::InvalidFrameState))?;
        let response = self.rpc.call_json(&address, &document)?.await?;
        if response.as_str()? != "{}" {
            return Err(EmitError::Rpc(RpcError::InvalidFrameState));
        }
        Ok(())
    }
}

struct EmitFields<'a, J: ?Sized> {
    event: &'a str,
    topic: Option<&'a Topic>,
    input: &'a J,
}

impl<J> JsonObjectFields for EmitFields<'_, J>
where
    J: JsonPayload + ?Sized,
{
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.string_field("event", self.event)?;
        if let Some(topic) = self.topic {
            writer.string_field("topic", topic.as_str())?;
        }
        writer.field("input", self.input)
    }
}

#[cfg(test)]
mod tests {
    #![allow(missing_docs)]

    use super::{EventId, EventIdError};

    #[test]
    fn event_id_accepts_flat_and_dotted_identifiers() {
        assert!(EventId::try_from("aabbbbccc").is_ok());
        assert!(EventId::try_from("aaa.bbb").is_ok());
        assert!(EventId::try_from("device-1_status.2").is_ok());
    }

    #[test]
    fn event_id_rejects_empty_or_pattern_values() {
        assert_eq!(EventId::try_from(""), Err(EventIdError::Empty));
        assert!(matches!(
            EventId::try_from("gateway.*"),
            Err(EventIdError::InvalidCharacter { .. })
        ));
    }

    #[test]
    fn event_id_rejects_non_identifier_characters() {
        for value in ["aaa bbb", "aaa/bbb", "事件"] {
            assert!(matches!(
                EventId::try_from(value),
                Err(EventIdError::InvalidCharacter { .. })
            ));
        }
    }
}
