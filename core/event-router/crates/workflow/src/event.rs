//! Public typed Event declaration and emission API.

use alloc::string::String;
use core::fmt;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;

use barracuda_rpc::{
    RpcClient, RpcError, RpcInputMode, RpcMessage, RpcResult, RpcStream, Streaming, Unary,
};

use super::{ingress, Topic};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventCardinality {
    Unary,
    Streaming,
}

mod private {
    use super::{EventCardinality, RpcInputMode, RpcMessage, RpcStream};

    pub trait InputMode<T>: RpcInputMode<T>
    where
        T: RpcMessage,
    {
        fn cardinality() -> EventCardinality;

        fn into_stream(input: <Self as RpcInputMode<T>>::ClientInput) -> RpcStream<T>;
    }
}

/// Type-level input cardinality accepted by one [`Event`].
///
/// This trait is sealed; use RPC's existing [`Unary`] and [`Streaming`]
/// marker types.
pub trait EventInputMode<T>: RpcInputMode<T> + private::InputMode<T>
where
    T: RpcMessage,
{
}

impl<T> private::InputMode<T> for Unary
where
    T: RpcMessage,
{
    fn cardinality() -> EventCardinality {
        EventCardinality::Unary
    }

    fn into_stream(input: <Self as RpcInputMode<T>>::ClientInput) -> RpcStream<T> {
        RpcStream::new(OnceEventStream { value: Some(input) })
    }
}

impl<T> EventInputMode<T> for Unary where T: RpcMessage {}

impl<T> private::InputMode<T> for Streaming
where
    T: RpcMessage,
{
    fn cardinality() -> EventCardinality {
        EventCardinality::Streaming
    }

    fn into_stream(input: <Self as RpcInputMode<T>>::ClientInput) -> RpcStream<T> {
        input
    }
}

impl<T> EventInputMode<T> for Streaming where T: RpcMessage {}

/// Compile-time description of one typed Event.
pub trait Event: 'static {
    /// Stable Event identifier used by Workflow matching.
    const ID: &'static str;

    /// Fixed-layout Event payload message.
    type Message: RpcMessage;

    /// Unary or streaming Event input.
    type Input: EventInputMode<Self::Message>;
}

pub(super) fn cardinality<E>() -> EventCardinality
where
    E: Event,
{
    <E::Input as private::InputMode<E::Message>>::cardinality()
}

pub(super) fn into_stream<E>(
    input: <E::Input as RpcInputMode<E::Message>>::ClientInput,
) -> RpcStream<E::Message>
where
    E: Event,
{
    <E::Input as private::InputMode<E::Message>>::into_stream(input)
}

/// Receiver-side reason an Event was rejected before ownership transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmitRejection {
    /// First frame is absent or not a valid Header.
    InvalidHeader,
    /// Header Event ID is invalid.
    InvalidEventId,
    /// Header Event ID exceeds the wire limit.
    EventIdTooLong,
    /// Header cardinality is unknown.
    InvalidCardinality,
    /// Event message wire size is zero or unrepresentable.
    InvalidMessageSize,
    /// Payload frame metadata or padding is invalid.
    InvalidPayloadFrame,
    /// Request EOF occurred partway through a message.
    TruncatedPayload,
    /// Unary Event did not contain exactly one message.
    InvalidUnaryMessageCount,
    /// Matched Workflow inputs are unavailable or incompatible.
    DownstreamUnavailable,
    /// Topic metadata is missing or not a valid fixed C string.
    InvalidTopic,
    /// The fixed Topic metadata does not fit in the Router frame.
    TopicTooLong,
    /// Error code is not recognized by the ingress protocol.
    Unknown,
}

/// Failure returned by [`EventEmitter::emit`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmitError {
    /// The Event type declares an invalid Event ID.
    #[error("invalid Event ID: {0}")]
    InvalidEventId(#[from] EventIdError),
    /// Local Event metadata cannot be represented by the ingress protocol.
    #[error("Event cannot be encoded: {0:?}")]
    Encoding(EmitRejection),
    /// RPC transport or runtime failure.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// WorkflowRuntime rejected the Event before ownership transfer.
    #[error("Event was rejected: {0:?}")]
    Rejected(EmitRejection),
}

/// Typed Event producer backed by a Component's RPC client.
#[derive(Clone)]
pub struct EventEmitter<const M: usize> {
    rpc: RpcClient,
}

impl<const M: usize> EventEmitter<M> {
    /// Wraps an existing client without creating another registry or transport.
    #[must_use]
    pub const fn new(rpc: RpcClient) -> Self {
        const { ingress::assert_frame_capacity::<M>() }
        Self { rpc }
    }

    /// Emits one typed Event and waits until WorkflowRuntime accepts ownership.
    ///
    /// This does not wait for matching Workflow executions or downstream RPCs
    /// to complete.
    ///
    /// An Event ID that cannot fit in the Router's frame capacity is rejected
    /// at compile time:
    ///
    /// ```compile_fail
    /// use barracuda_rpc::Unary;
    /// use barracuda_workflow::{Event, EventEmitter};
    ///
    /// struct EventIdExceedsFrame;
    ///
    /// impl Event for EventIdExceedsFrame {
    ///     const ID: &'static str =
    ///         "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    ///     type Message = [u8; 1];
    ///     type Input = Unary;
    /// }
    ///
    /// fn emitter() -> &'static EventEmitter<64> {
    ///     loop {}
    /// }
    ///
    /// fn main() {
    ///     let _ = emitter().emit::<EventIdExceedsFrame>([0]);
    /// }
    /// ```
    pub async fn emit<E>(
        &self,
        input: <E::Input as RpcInputMode<E::Message>>::ClientInput,
    ) -> Result<(), EmitError>
    where
        E: Event,
    {
        ingress::emit::<E, M>(&self.rpc, None, input).await
    }

    /// Emits one typed Event with a topic used by optional Workflow filtering.
    ///
    /// Workflows without `match.topic` still receive this Event. Workflows with
    /// `match.topic` receive it only when their exact topic matches `topic`.
    pub async fn emit_to<E>(
        &self,
        topic: &Topic,
        input: <E::Input as RpcInputMode<E::Message>>::ClientInput,
    ) -> Result<(), EmitError>
    where
        E: Event,
    {
        ingress::emit::<E, M>(&self.rpc, Some(topic), input).await
    }
}

struct OnceEventStream<T> {
    value: Option<T>,
}

impl<T> Unpin for OnceEventStream<T> {}

impl<T> Stream for OnceEventStream<T> {
    type Item = RpcResult<T>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().value.take().map(Ok))
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
            EventId::try_from("aaa.*"),
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
