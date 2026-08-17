//! Private RPC ingress used to deliver typed Events to Workflow Runtime.

use core::mem::size_of;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;
use getset::Getters;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use barracuda_rpc::{
    RpcClient, RpcError, RpcFrame, RpcInputMode, RpcMessage, RpcMethod, RpcPayloadWriter,
    RpcResult, RpcStream, Streaming, Unary,
};

use super::event::{
    cardinality, into_stream, EmitError, EmitRejection, Event, EventCardinality, EventId,
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
enum InternalEmitFrameKind {
    Header,
    Payload,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
enum InternalEmitCardinality {
    None,
    Unary,
    Streaming,
}

impl From<EventCardinality> for InternalEmitCardinality {
    fn from(cardinality: EventCardinality) -> Self {
        match cardinality {
            EventCardinality::Unary => Self::Unary,
            EventCardinality::Streaming => Self::Streaming,
        }
    }
}

impl TryFrom<InternalEmitCardinality> for EventCardinality {
    type Error = EmitRejection;

    fn try_from(cardinality: InternalEmitCardinality) -> Result<Self, Self::Error> {
        match cardinality {
            InternalEmitCardinality::Unary => Ok(Self::Unary),
            InternalEmitCardinality::Streaming => Ok(Self::Streaming),
            InternalEmitCardinality::None => Err(EmitRejection::InvalidCardinality),
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct InternalEmitHeader {
    kind: InternalEmitFrameKind,
    cardinality: InternalEmitCardinality,
    event_id_length: usize,
    data_length: usize,
    message_size: usize,
}

/// One fixed-layout frame carried by the unique `internal.emit` RPC.
///
/// `M` is inherited from Event Router's RPC lane capacity. Header metadata
/// occupies [`size_of::<InternalEmitHeader>()`], and all remaining bytes carry
/// either the Event ID or opaque Event payload.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct InternalEmitFrame<const M: usize> {
    bytes: [u8; M],
}

impl<const M: usize> InternalEmitFrame<M> {
    const fn data_capacity() -> usize {
        M.saturating_sub(size_of::<InternalEmitHeader>())
    }

    fn new_header(
        event_id: &EventId,
        cardinality: EventCardinality,
        message_size: usize,
    ) -> Result<Self, EmitRejection> {
        let event_id_bytes = event_id.as_str().as_bytes();
        if event_id_bytes.len() > Self::data_capacity() {
            return Err(EmitRejection::EventIdTooLong);
        }
        if message_size == 0 {
            return Err(EmitRejection::InvalidMessageSize);
        }
        Self::encode(
            InternalEmitHeader {
                kind: InternalEmitFrameKind::Header,
                cardinality: cardinality.into(),
                event_id_length: event_id_bytes.len(),
                data_length: 0,
                message_size,
            },
            event_id_bytes,
            EmitRejection::EventIdTooLong,
        )
    }

    fn new_payload(bytes: &[u8]) -> Result<Self, EmitRejection> {
        if bytes.is_empty() || bytes.len() > Self::data_capacity() {
            return Err(EmitRejection::InvalidPayloadFrame);
        }
        Self::encode(
            InternalEmitHeader {
                kind: InternalEmitFrameKind::Payload,
                cardinality: InternalEmitCardinality::None,
                event_id_length: 0,
                data_length: bytes.len(),
                message_size: 0,
            },
            bytes,
            EmitRejection::InvalidPayloadFrame,
        )
    }

    fn encode(
        header: InternalEmitHeader,
        data: &[u8],
        rejection: EmitRejection,
    ) -> Result<Self, EmitRejection> {
        let mut frame = Self { bytes: [0; M] };
        let header_size = size_of::<InternalEmitHeader>();
        let header_bytes = frame.bytes.get_mut(..header_size).ok_or(rejection)?;
        header_bytes.copy_from_slice(header.as_bytes());
        let data_end = header_size.checked_add(data.len()).ok_or(rejection)?;
        let destination = frame
            .bytes
            .get_mut(header_size..data_end)
            .ok_or(rejection)?;
        destination.copy_from_slice(data);
        Ok(frame)
    }

    fn decode(
        &self,
        rejection: EmitRejection,
    ) -> Result<(InternalEmitHeader, &[u8]), EmitRejection> {
        InternalEmitHeader::try_read_from_prefix(&self.bytes).map_err(|_error| rejection)
    }

    /// Decodes this frame as the required first Header frame.
    pub(super) fn header(&self) -> Result<EmitHeader, EmitRejection> {
        let (header, data) = self.decode(EmitRejection::InvalidHeader)?;
        if header.kind != InternalEmitFrameKind::Header || header.data_length != 0 {
            return Err(EmitRejection::InvalidHeader);
        }
        if header.event_id_length == 0 || header.event_id_length > data.len() {
            return Err(EmitRejection::InvalidEventId);
        }
        let event_id_bytes = data
            .get(..header.event_id_length)
            .ok_or(EmitRejection::InvalidEventId)?;
        let event_id_text =
            core::str::from_utf8(event_id_bytes).map_err(|_error| EmitRejection::InvalidEventId)?;
        let event_id =
            EventId::try_from(event_id_text).map_err(|_error| EmitRejection::InvalidEventId)?;
        if data
            .get(header.event_id_length..)
            .is_none_or(|unused| unused.iter().any(|byte| *byte != 0))
        {
            return Err(EmitRejection::InvalidHeader);
        }
        if header.message_size == 0 {
            return Err(EmitRejection::InvalidMessageSize);
        }
        Ok(EmitHeader {
            event_id,
            cardinality: header.cardinality.try_into()?,
            message_size: header.message_size,
        })
    }

    /// Decodes this frame as one opaque Payload frame.
    pub fn payload(&self) -> Result<&[u8], EmitRejection> {
        let (header, data) = self.decode(EmitRejection::InvalidPayloadFrame)?;
        if header.kind != InternalEmitFrameKind::Payload
            || header.cardinality != InternalEmitCardinality::None
            || header.event_id_length != 0
            || header.message_size != 0
        {
            return Err(EmitRejection::InvalidPayloadFrame);
        }
        if header.data_length == 0 || header.data_length > data.len() {
            return Err(EmitRejection::InvalidPayloadFrame);
        }
        let payload = data
            .get(..header.data_length)
            .ok_or(EmitRejection::InvalidPayloadFrame)?;
        if data
            .get(header.data_length..)
            .is_none_or(|unused| unused.iter().any(|byte| *byte != 0))
        {
            return Err(EmitRejection::InvalidPayloadFrame);
        }
        Ok(payload)
    }
}

/// Decoded metadata from an internal Event Header frame.
#[derive(Clone, Debug, Getters, PartialEq, Eq)]
pub(super) struct EmitHeader {
    /// Event ID used by the Workflow matcher.
    #[getset(get = "pub(super)")]
    event_id: EventId,
    cardinality: EventCardinality,
    message_size: usize,
}

/// Receiver-side view of one accepted `internal.emit` request.
///
/// WorkflowRuntime can inspect [`header`](Self::header) to match the Event ID,
/// then use [`forward_to`](Self::forward_to) to copy each opaque Event message
/// directly into a multicast request lane. Business payload bytes are never
/// decoded or collected in an intermediate buffer.
#[must_use = "an accepted Event request must be forwarded or discarded"]
#[derive(Getters)]
pub(super) struct InternalEmitRequest<const M: usize> {
    /// Metadata used for Workflow rule matching and ingress selection.
    #[getset(get = "pub(super)")]
    header: EmitHeader,
    frames: RpcStream<RpcFrame<InternalEmitFrame<M>>>,
}

impl<const M: usize> InternalEmitRequest<M> {
    /// Reads and validates the first `internal.emit` Header frame.
    ///
    /// Protocol rejection is returned as the Method's typed error; malformed
    /// RPC framing remains an outer transport error.
    pub(super) async fn accept(
        mut frames: RpcStream<RpcFrame<InternalEmitFrame<M>>>,
    ) -> RpcResult<Result<Self, EmitErrorFrame>> {
        let Some(frame) = frames.next().await else {
            return Ok(Err(EmitErrorFrame::new(EmitRejection::InvalidHeader)));
        };
        let frame = frame?;
        let header = match frame.view()?.header() {
            Ok(header) => header,
            Err(rejection) => return Ok(Err(EmitErrorFrame::new(rejection))),
        };
        Ok(Ok(Self { header, frames }))
    }

    /// Forwards opaque Event messages into a prepared unicast or multicast
    /// request writer, then closes its request direction.
    ///
    /// Every destination reads the same shared request frame. This method only
    /// copies bytes from the outer `internal.emit` envelope into that shared
    /// frame once; it does not decode the Event's business payload.
    pub(super) async fn forward_to(
        mut self,
        writer: &mut RpcPayloadWriter,
    ) -> RpcResult<Result<(), EmitErrorFrame>> {
        let mut message_count = 0usize;
        while let Some(frame) = self.frames.next().await {
            message_count = message_count
                .checked_add(1)
                .ok_or(RpcError::InvalidFrameState)?;
            let mut output = writer.reserve().await?;
            if output.as_mut().len() != self.header.message_size {
                return Ok(Err(EmitErrorFrame::new(
                    EmitRejection::DownstreamUnavailable,
                )));
            }

            let mut written = 0usize;
            let mut next_frame = Some(frame?);
            while written < self.header.message_size {
                let frame = match next_frame.take() {
                    Some(frame) => frame,
                    None => match self.frames.next().await {
                        Some(frame) => frame?,
                        None => {
                            return Ok(Err(EmitErrorFrame::new(EmitRejection::TruncatedPayload)));
                        }
                    },
                };
                let frame = frame.view()?;
                let payload = match frame.payload() {
                    Ok(payload) => payload,
                    Err(rejection) => return Ok(Err(EmitErrorFrame::new(rejection))),
                };
                let remaining = self
                    .header
                    .message_size
                    .checked_sub(written)
                    .ok_or(RpcError::InvalidFrameState)?;
                if payload.len() > remaining {
                    return Ok(Err(EmitErrorFrame::new(EmitRejection::InvalidPayloadFrame)));
                }
                let end = written
                    .checked_add(payload.len())
                    .ok_or(RpcError::InvalidFrameState)?;
                let destination = output
                    .as_mut()
                    .get_mut(written..end)
                    .ok_or(RpcError::InvalidFrameState)?;
                destination.copy_from_slice(payload);
                written = end;
            }

            if self.header.cardinality == EventCardinality::Unary {
                match self.frames.next().await {
                    Some(Ok(_extra_frame)) => {
                        return Ok(Err(EmitErrorFrame::new(
                            EmitRejection::InvalidUnaryMessageCount,
                        )));
                    }
                    Some(Err(error)) => return Err(error),
                    None => {}
                }
            }
            output.commit(written)?;
        }

        if self.header.cardinality == EventCardinality::Unary && message_count != 1 {
            return Ok(Err(EmitErrorFrame::new(
                EmitRejection::InvalidUnaryMessageCount,
            )));
        }
        writer.close().await?;
        Ok(Ok(()))
    }

    /// Validates and consumes an unmatched Event without decoding its payload.
    pub(super) async fn discard(mut self) -> RpcResult<Result<(), EmitErrorFrame>> {
        let mut message_count = 0usize;
        let mut message_bytes = 0usize;
        while let Some(frame) = self.frames.next().await {
            let frame = frame?;
            let frame = frame.view()?;
            let payload = match frame.payload() {
                Ok(payload) => payload,
                Err(rejection) => return Ok(Err(EmitErrorFrame::new(rejection))),
            };
            message_bytes = message_bytes
                .checked_add(payload.len())
                .ok_or(RpcError::InvalidFrameState)?;
            if message_bytes > self.header.message_size {
                return Ok(Err(EmitErrorFrame::new(EmitRejection::InvalidPayloadFrame)));
            }
            if message_bytes == self.header.message_size {
                message_count = message_count
                    .checked_add(1)
                    .ok_or(RpcError::InvalidFrameState)?;
                message_bytes = 0;
            }
        }
        if message_bytes != 0 {
            return Ok(Err(EmitErrorFrame::new(EmitRejection::TruncatedPayload)));
        }
        if self.header.cardinality == EventCardinality::Unary && message_count != 1 {
            return Ok(Err(EmitErrorFrame::new(
                EmitRejection::InvalidUnaryMessageCount,
            )));
        }
        Ok(Ok(()))
    }
}

impl EmitRejection {
    const fn code(self) -> u8 {
        match self {
            Self::InvalidHeader => 1,
            Self::InvalidEventId => 2,
            Self::EventIdTooLong => 3,
            Self::InvalidCardinality => 4,
            Self::InvalidMessageSize => 5,
            Self::InvalidPayloadFrame => 6,
            Self::TruncatedPayload => 7,
            Self::InvalidUnaryMessageCount => 8,
            Self::DownstreamUnavailable => 9,
            Self::Unknown => u8::MAX,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::InvalidHeader,
            2 => Self::InvalidEventId,
            3 => Self::EventIdTooLong,
            4 => Self::InvalidCardinality,
            5 => Self::InvalidMessageSize,
            6 => Self::InvalidPayloadFrame,
            7 => Self::TruncatedPayload,
            8 => Self::InvalidUnaryMessageCount,
            9 => Self::DownstreamUnavailable,
            _ => Self::Unknown,
        }
    }
}

/// Fixed-layout Method error returned by `internal.emit`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct EmitErrorFrame {
    code: u8,
}

impl EmitErrorFrame {
    /// Encodes one receiver rejection.
    #[must_use]
    pub(super) const fn new(rejection: EmitRejection) -> Self {
        Self {
            code: rejection.code(),
        }
    }

    /// Decodes the rejection reason.
    #[must_use]
    fn rejection(&self) -> EmitRejection {
        EmitRejection::from_code(self.code)
    }
}

/// The unique typed RPC used by all [`crate::EventEmitter`] instances.
pub struct InternalEmit<const M: usize>;

impl<const M: usize> RpcMethod for InternalEmit<M> {
    const ADDRESS: &'static str = "internal.emit";
    type Request = InternalEmitFrame<M>;
    type Response = ();
    type Error = EmitErrorFrame;
    type Input = Streaming;
    type Output = Unary;
}

pub(super) const fn assert_frame_capacity<const M: usize>() {
    assert!(
        M > size_of::<InternalEmitHeader>(),
        "Event emitter requires room for its header and at least one data byte"
    );
}

pub(super) async fn emit<E, const M: usize>(
    rpc: &RpcClient,
    input: <E::Input as RpcInputMode<E::Message>>::ClientInput,
) -> Result<(), EmitError>
where
    E: Event,
{
    const {
        assert!(
            E::ID.len() <= InternalEmitFrame::<M>::data_capacity(),
            "Event ID exceeds Event Router frame capacity"
        );
    }
    let event_id = EventId::try_from(E::ID)?;
    let header =
        InternalEmitFrame::<M>::new_header(&event_id, cardinality::<E>(), size_of::<E::Message>())
            .map_err(EmitError::Encoding)?;
    let frames = RpcStream::new(EncodedEventStream::new(header, into_stream::<E>(input)));
    let outcome = rpc.call::<InternalEmit<M>>(frames)?.await?;
    match outcome {
        Ok(accepted) => {
            accepted.view()?;
            Ok(())
        }
        Err(error) => {
            let rejection = error.view().map_err(EmitError::Rpc)?.rejection();
            Err(EmitError::Rejected(rejection))
        }
    }
}

struct EncodedEventStream<T, const M: usize> {
    header: Option<InternalEmitFrame<M>>,
    messages: RpcStream<T>,
    current: Option<T>,
    offset: usize,
}

impl<T, const M: usize> EncodedEventStream<T, M> {
    fn new(header: InternalEmitFrame<M>, messages: RpcStream<T>) -> Self {
        Self {
            header: Some(header),
            messages,
            current: None,
            offset: 0,
        }
    }
}

impl<T, const M: usize> Unpin for EncodedEventStream<T, M> {}

impl<T, const M: usize> Stream for EncodedEventStream<T, M>
where
    T: RpcMessage,
{
    type Item = RpcResult<InternalEmitFrame<M>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(header) = this.header.take() {
            return Poll::Ready(Some(Ok(header)));
        }

        loop {
            if let Some(message) = &this.current {
                let bytes = message.as_bytes();
                let remaining = match bytes.get(this.offset..) {
                    Some(remaining) => remaining,
                    None => return Poll::Ready(Some(Err(RpcError::InvalidFrameState))),
                };
                let length = remaining.len().min(InternalEmitFrame::<M>::data_capacity());
                let Some(chunk) = remaining.get(..length) else {
                    return Poll::Ready(Some(Err(RpcError::InvalidFrameState)));
                };
                let frame = match InternalEmitFrame::<M>::new_payload(chunk) {
                    Ok(frame) => frame,
                    Err(_error) => {
                        return Poll::Ready(Some(Err(RpcError::InvalidFrameState)));
                    }
                };
                let Some(next_offset) = this.offset.checked_add(length) else {
                    return Poll::Ready(Some(Err(RpcError::InvalidFrameState)));
                };
                this.offset = next_offset;
                if this.offset == bytes.len() {
                    this.current = None;
                    this.offset = 0;
                }
                return Poll::Ready(Some(Ok(frame)));
            }

            match Pin::new(&mut this.messages).poll_next(context) {
                Poll::Ready(Some(Ok(message))) => this.current = Some(message),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    use futures_lite::future::block_on;
    use futures_util::stream;

    use super::*;
    use crate::{EventEmitter, Rule};
    use barracuda_rpc::{
        RpcAddress, RpcContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcStream,
        Streaming, Unary,
    };

    #[test]
    fn internal_emit_frame_uses_router_frame_capacity() {
        assert_eq!(size_of::<InternalEmitFrame<64>>(), 64);
        assert_eq!(size_of::<InternalEmitFrame<300>>(), 300);
        assert_eq!(size_of::<EmitErrorFrame>(), size_of::<u8>());
        assert_eq!(
            InternalEmitFrame::<64>::data_capacity(),
            64 - size_of::<InternalEmitHeader>()
        );
        assert_eq!(
            InternalEmitFrame::<300>::data_capacity(),
            300 - size_of::<InternalEmitHeader>()
        );
    }

    #[test]
    fn event_id_and_payload_share_the_capacity_remaining_after_the_header() {
        let capacity = InternalEmitFrame::<64>::data_capacity();
        let event_id = EventId::try_from("a".repeat(capacity)).expect("valid Event ID");
        let header = InternalEmitFrame::<64>::new_header(
            &event_id,
            EventCardinality::Unary,
            size_of::<u32>(),
        )
        .expect("encode maximum Event ID");
        assert_eq!(
            header.header().expect("decode header").event_id(),
            &event_id
        );

        let payload = alloc::vec![0x5a; capacity];
        let frame =
            InternalEmitFrame::<64>::new_payload(&payload).expect("encode maximum payload frame");
        assert_eq!(frame.payload().expect("decode payload"), payload);

        let oversized = alloc::vec![0x5a; capacity + 1];
        assert!(matches!(
            InternalEmitFrame::<64>::new_payload(&oversized),
            Err(EmitRejection::InvalidPayloadFrame)
        ));
    }

    struct LargeUnaryEvent;

    impl Event for LargeUnaryEvent {
        const ID: &'static str = "gateway.message.received";
        type Message = [u8; 300];
        type Input = Unary;
    }

    struct SmallStreamingEvent;

    impl Event for SmallStreamingEvent {
        const ID: &'static str = "scheduler.triggered";
        type Message = [u8; 4];
        type Input = Streaming;
    }

    struct InvalidIdEvent;

    impl Event for InvalidIdEvent {
        const ID: &'static str = "invalid.*";
        type Message = [u8; 1];
        type Input = Unary;
    }

    struct WorkflowIngressA;

    impl RpcMethod for WorkflowIngressA {
        const ADDRESS: &'static str = "workflow.alpha";
        type Request = [u8; 300];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct WorkflowIngressB;

    impl RpcMethod for WorkflowIngressB {
        const ADDRESS: &'static str = "workflow.beta";
        type Request = [u8; 300];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    #[test]
    fn unary_event_encodes_header_and_fragmented_payload_before_success() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 256, 1>::new()));
            let registry = RpcRegistry::new(lanes);
            let received = Rc::new(RefCell::new(Vec::new()));
            let handler_received = Rc::clone(&received);
            registry
                .register::<InternalEmit<256>, _>(
                    move |_context, mut frames: RpcStream<RpcFrame<InternalEmitFrame<256>>>| {
                        let received = Rc::clone(&handler_received);
                        async move {
                            while let Some(frame) = frames.next().await {
                                received.borrow_mut().push(*frame?.view()?);
                            }
                            Ok(Ok(()))
                        }
                    },
                )
                .expect("register internal emit");

            let input = [0x5a; 300];
            EventEmitter::<256>::new(registry.client())
                .emit::<LargeUnaryEvent>(input)
                .await
                .expect("emit unary event");

            let frames = received.borrow();
            let mut frames = frames.iter();
            let header = frames
                .next()
                .expect("header frame")
                .header()
                .expect("decode header");
            assert_eq!(header.event_id().as_str(), LargeUnaryEvent::ID);
            assert_eq!(header.cardinality, EventCardinality::Unary);
            assert_eq!(header.message_size, 300);

            let mut payload = Vec::new();
            for frame in frames {
                payload.extend_from_slice(frame.payload().expect("decode payload"));
            }
            assert_eq!(payload, input);
        });
    }

    #[test]
    fn streaming_event_preserves_message_order_and_allows_empty_stream() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 256, 1>::new()));
            let registry = RpcRegistry::new(lanes);
            let received = Rc::new(RefCell::new(Vec::new()));
            let handler_received = Rc::clone(&received);
            registry
                .register::<InternalEmit<256>, _>(
                    move |_context, mut frames: RpcStream<RpcFrame<InternalEmitFrame<256>>>| {
                        let received = Rc::clone(&handler_received);
                        async move {
                            while let Some(frame) = frames.next().await {
                                received.borrow_mut().push(*frame?.view()?);
                            }
                            Ok(Ok(()))
                        }
                    },
                )
                .expect("register internal emit");
            let emitter = EventEmitter::<256>::new(registry.client());

            let input = RpcStream::new(stream::iter([Ok([1, 2, 3, 4]), Ok([5, 6, 7, 8])]));
            emitter
                .emit::<SmallStreamingEvent>(input)
                .await
                .expect("emit streaming event");

            {
                let frames = received.borrow();
                let mut frames = frames.iter();
                let header = frames
                    .next()
                    .expect("streaming header")
                    .header()
                    .expect("decode streaming header");
                assert_eq!(header.cardinality, EventCardinality::Streaming);
                assert_eq!(
                    frames
                        .next()
                        .expect("first payload frame")
                        .payload()
                        .expect("first payload"),
                    &[1, 2, 3, 4]
                );
                assert_eq!(
                    frames
                        .next()
                        .expect("second payload frame")
                        .payload()
                        .expect("second payload"),
                    &[5, 6, 7, 8]
                );
                assert!(frames.next().is_none());
            }

            received.borrow_mut().clear();
            emitter
                .emit::<SmallStreamingEvent>(RpcStream::new(stream::empty()))
                .await
                .expect("emit empty stream");
            let frames = received.borrow();
            let mut frames = frames.iter();
            assert!(frames.next().is_some_and(|frame| frame.header().is_ok()));
            assert!(frames.next().is_none());
        });
    }

    #[test]
    fn invalid_event_id_is_rejected_before_rpc_io() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 256, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let called = Rc::new(Cell::new(false));
        let handler_called = Rc::clone(&called);
        registry
            .register::<InternalEmit<256>, _>(
                move |_context, _frames: RpcStream<RpcFrame<InternalEmitFrame<256>>>| {
                    handler_called.set(true);
                    async { Ok(Ok(())) }
                },
            )
            .expect("register internal emit");

        let result =
            block_on(EventEmitter::<256>::new(registry.client()).emit::<InvalidIdEvent>([1]));
        assert!(matches!(result, Err(EmitError::InvalidEventId(_))));
        assert!(!called.get());
    }

    #[test]
    fn receiver_rejection_is_restored_as_emit_error() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 256, 1>::new()));
            let registry = RpcRegistry::new(lanes);
            registry
                .register::<InternalEmit<256>, _>(
                    |_context, _frames: RpcStream<RpcFrame<InternalEmitFrame<256>>>| async {
                        Ok(Err(EmitErrorFrame::new(EmitRejection::InvalidPayloadFrame)))
                    },
                )
                .expect("register rejecting internal emit");

            let error = EventEmitter::<256>::new(registry.client())
                .emit::<LargeUnaryEvent>([0; 300])
                .await
                .expect_err("receiver rejection");
            assert!(matches!(
                error,
                EmitError::Rejected(EmitRejection::InvalidPayloadFrame)
            ));
        });
    }

    #[test]
    fn internal_emit_matches_event_id_then_multicasts_opaque_message_once() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<3, 300, 3>::new()));
            let registry = RpcRegistry::new(lanes);
            let observations = Rc::new(RefCell::new(Vec::new()));

            for address in [WorkflowIngressA::ADDRESS, WorkflowIngressB::ADDRESS] {
                let observations = Rc::clone(&observations);
                let handler = move |_context, frame: RpcFrame<[u8; 300]>| {
                    let observations = Rc::clone(&observations);
                    async move {
                        let message = frame.view()?;
                        observations
                            .borrow_mut()
                            .push((message.as_ptr() as usize, *message));
                        Ok(Ok(()))
                    }
                };
                if address == WorkflowIngressA::ADDRESS {
                    registry
                        .register::<WorkflowIngressA, _>(handler)
                        .expect("register workflow alpha");
                } else {
                    registry
                        .register::<WorkflowIngressB, _>(handler)
                        .expect("register workflow beta");
                }
            }

            let routes = Rc::new([
                (
                    Rule::try_from("gateway.*").expect("valid alpha rule"),
                    RpcAddress::try_from(WorkflowIngressA::ADDRESS).expect("valid alpha address"),
                ),
                (
                    Rule::try_from("gateway.message.received").expect("valid beta rule"),
                    RpcAddress::try_from(WorkflowIngressB::ADDRESS).expect("valid beta address"),
                ),
            ]);
            registry
                .register::<InternalEmit<300>, _>(move |context: RpcContext, frames| {
                    let routes = Rc::clone(&routes);
                    async move {
                        let request = match InternalEmitRequest::accept(frames).await? {
                            Ok(request) => request,
                            Err(rejection) => return Ok(Err(rejection)),
                        };
                        let addresses: Vec<_> = routes
                            .iter()
                            .filter(|(rule, _address)| rule.matches(request.header().event_id()))
                            .map(|(_rule, address)| address.clone())
                            .collect();
                        let (mut writer, branches) =
                            context.client().multicast_payload(&addresses)?;
                        let outcome = request.forward_to(&mut writer).await?;
                        drop(branches);
                        Ok(outcome)
                    }
                })
                .expect("register internal emit");

            let input = [0x3c; 300];
            EventEmitter::<300>::new(registry.client())
                .emit::<LargeUnaryEvent>(input)
                .await
                .expect("emit matched event");

            let observations = observations.borrow();
            let mut observations = observations.iter();
            let first = observations.next().expect("first workflow observation");
            let second = observations.next().expect("second workflow observation");
            assert!(observations.next().is_none());
            assert_eq!(first.1, input);
            assert_eq!(second.1, input);
            assert_eq!(first.0, second.0);
        });
    }
}
