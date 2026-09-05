//! Private JSON RPC ingress used to deliver Events to Workflow Runtime.

use core::ops::Range;

use barracuda_rpc::{JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, RpcError, RpcResult};
use getset::Getters;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{EventId, Topic};

const INTERNAL_EMIT_REQUEST_SCHEMA: JsonSchema = JsonSchema::new(
    r#"{"type":"object","properties":{"event":{"type":"string"},"topic":{"type":"string"},"input":{}},"required":["event","input"],"additionalProperties":false}"#,
);
const EMPTY_RESPONSE_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);

/// The unique JSON RPC used by all [`crate::EventEmitter`] instances.
pub struct InternalEmit<const M: usize>;

impl<const M: usize> JsonRpcSchema for InternalEmit<M> {
    const ADDRESS: &'static str = "internal.emit";
    const REQUEST_SCHEMA: JsonSchema = INTERNAL_EMIT_REQUEST_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_RESPONSE_SCHEMA;
    const MAX_REQUEST_BYTES: usize = M;
    const MAX_RESPONSE_BYTES: usize = 2;
}

/// Decoded metadata from one internal Event document.
#[derive(Clone, Debug, Getters, PartialEq, Eq)]
pub(super) struct EmitHeader {
    /// Event ID used by Workflow matching.
    #[getset(get = "pub(super)")]
    event_id: EventId,
    topic: Option<Topic>,
}

impl EmitHeader {
    pub(super) const fn topic(&self) -> Option<&Topic> {
        self.topic.as_ref()
    }
}

/// One Event input retained in its internal RPC request lane.
pub(super) struct EventInput {
    request: JsonRef,
    input: Range<usize>,
}

impl EventInput {
    /// Validates the internal envelope and locates its borrowed input JSON.
    pub(super) fn accept(request: JsonRef) -> RpcResult<(EmitHeader, Self)> {
        let source = request.as_str()?;
        let document: EmitDocument<'_> =
            serde_json::from_str(source).map_err(|_error| RpcError::InvalidJson)?;
        let event_id = EventId::try_from(document.event).map_err(|_error| RpcError::InvalidJson)?;
        let topic = document
            .topic
            .map(Topic::try_from)
            .transpose()
            .map_err(|_error| RpcError::InvalidJson)?;
        let input = document.input.get();
        let start = (input.as_ptr() as usize)
            .checked_sub(source.as_ptr() as usize)
            .ok_or(RpcError::InvalidFrameState)?;
        let end = start
            .checked_add(input.len())
            .ok_or(RpcError::InvalidFrameState)?;
        if source.get(start..end).is_none() {
            return Err(RpcError::InvalidFrameState);
        }
        Ok((
            EmitHeader { event_id, topic },
            Self {
                request,
                input: start..end,
            },
        ))
    }

    /// Borrows the original Event input JSON directly from the request lane.
    pub(super) fn as_str(&self) -> RpcResult<&str> {
        self.request
            .as_str()?
            .get(self.input.clone())
            .ok_or(RpcError::InvalidFrameState)
    }
}

impl JsonPayload for EventInput {
    fn encoded_len(&self) -> RpcResult<usize> {
        Ok(self.as_str()?.len())
    }

    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize> {
        self.as_str()?.write_json(destination)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmitDocument<'a> {
    event: &'a str,
    #[serde(default)]
    topic: Option<&'a str>,
    #[serde(borrow)]
    input: &'a RawValue,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::borrow::ToOwned;
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use core::cell::{Cell, RefCell};

    use barracuda_rpc::{JsonWriter, RpcLaneStorage, RpcRegistry};
    use futures_lite::future::block_on;

    use super::{EventInput, InternalEmit};
    use crate::{EmitError, Event, EventEmitter, Topic};

    struct MessageReceived;

    impl Event for MessageReceived {
        const ID: &'static str = "gateway.message.received";
    }

    struct InvalidId;

    impl Event for InvalidId {
        const ID: &'static str = "gateway.*";
    }

    #[test]
    fn event_emitter_sends_raw_json_and_optional_topic() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 256, 1>::new()));
            let registry = RpcRegistry::new(lanes);
            let seen = Rc::new(RefCell::new(None));
            let handler_seen = Rc::clone(&seen);
            registry
                .register_json::<InternalEmit<256>, _>(
                    "system",
                    move |_context, request, response: JsonWriter| {
                        let seen = Rc::clone(&handler_seen);
                        async move {
                            let (header, input) = EventInput::accept(request)?;
                            seen.replace(Some((
                                header.event_id().as_str().to_owned(),
                                header.topic().map(|topic| topic.as_str().to_owned()),
                                input.as_str()?.to_owned(),
                            )));
                            response.write("{}").await
                        }
                    },
                )
                .expect("register Event ingress");

            let topic = Topic::try_from("conversation-7").expect("valid Topic");
            EventEmitter::<256>::new(registry.client())
                .emit_to::<MessageReceived>(&topic, r#"{"message":"hello","conversation":"7"}"#)
                .await
                .expect("emit JSON Event");

            assert_eq!(
                seen.take(),
                Some((
                    "gateway.message.received".to_owned(),
                    Some("conversation-7".to_owned()),
                    r#"{"message":"hello","conversation":"7"}"#.to_owned(),
                ))
            );
        });
    }

    #[test]
    fn invalid_event_id_is_rejected_before_rpc_io() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let called = Rc::new(Cell::new(false));
        let handler_called = Rc::clone(&called);
        registry
            .register_json::<InternalEmit<128>, _>(
                "system",
                move |_context, _request, response: JsonWriter| {
                    handler_called.set(true);
                    async move { response.write("{}").await }
                },
            )
            .expect("register Event ingress");

        let result = block_on(EventEmitter::<128>::new(registry.client()).emit::<InvalidId>("{}"));
        assert!(matches!(result, Err(EmitError::InvalidEventId(_))));
        assert!(!called.get());
    }
}
