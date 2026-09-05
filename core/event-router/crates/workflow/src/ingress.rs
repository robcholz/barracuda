//! Private JSON RPC ingress used to deliver Events to Workflow Runtime.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Ref, RefCell};

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

struct EventSlot {
    bytes: Box<[u8]>,
    len: usize,
    occupied: bool,
}

/// Fixed Event-input storage allocated once by Event Router.
pub(super) struct EventInputPool {
    slots: Vec<RefCell<EventSlot>>,
}

impl EventInputPool {
    pub(super) fn new(slot_count: usize, slot_capacity: usize) -> Self {
        let slots = (0..slot_count)
            .map(|_| {
                RefCell::new(EventSlot {
                    bytes: alloc::vec![0; slot_capacity].into_boxed_slice(),
                    len: 0,
                    occupied: false,
                })
            })
            .collect();
        Self { slots }
    }

    pub(super) fn store(self: &Rc<Self>, input: &str) -> RpcResult<EventInput> {
        for (index, slot) in self.slots.iter().enumerate() {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                continue;
            };
            if slot.occupied {
                continue;
            }
            let capacity = slot.bytes.len();
            let destination = slot
                .bytes
                .get_mut(..input.len())
                .ok_or(RpcError::FrameTooLarge {
                    size: input.len(),
                    capacity,
                })?;
            destination.copy_from_slice(input.as_bytes());
            slot.len = input.len();
            slot.occupied = true;
            return Ok(EventInput {
                pool: Rc::clone(self),
                slot: index,
            });
        }
        Err(RpcError::ResourceExhausted {
            resource: "Workflow Event inputs",
            limit: self.slots.len(),
        })
    }
}

/// One Event input retained in Event Router's fixed buffer pool.
pub(super) struct EventInput {
    pool: Rc<EventInputPool>,
    slot: usize,
}

impl EventInput {
    /// Validates the internal envelope and copies its input into fixed storage.
    pub(super) fn accept(
        request: JsonRef,
        pool: &Rc<EventInputPool>,
    ) -> RpcResult<(EmitHeader, Self)> {
        let source = request.as_str()?;
        let document: EmitDocument<'_> =
            serde_json::from_str(source).map_err(|_error| RpcError::InvalidJson)?;
        let event_id = EventId::try_from(document.event).map_err(|_error| RpcError::InvalidJson)?;
        let topic = document
            .topic
            .map(Topic::try_from)
            .transpose()
            .map_err(|_error| RpcError::InvalidJson)?;
        let input = pool.store(document.input.get())?;
        Ok((EmitHeader { event_id, topic }, input))
    }

    /// Borrows the Event input JSON from its fixed storage slot.
    pub(super) fn as_str(&self) -> RpcResult<Ref<'_, str>> {
        let slot = self
            .pool
            .slots
            .get(self.slot)
            .ok_or(RpcError::InvalidFrameState)?
            .try_borrow()
            .map_err(|_error| RpcError::InvalidFrameState)?;
        Ref::filter_map(slot, |slot| {
            let bytes = slot.bytes.get(..slot.len)?;
            core::str::from_utf8(bytes).ok()
        })
        .map_err(|_slot| RpcError::InvalidFrameState)
    }
}

impl Drop for EventInput {
    fn drop(&mut self) {
        let Some(slot) = self.pool.slots.get(self.slot) else {
            return;
        };
        if let Ok(mut slot) = slot.try_borrow_mut() {
            slot.len = 0;
            slot.occupied = false;
        }
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

    use super::{EventInput, EventInputPool, InternalEmit};
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
            let pool = Rc::new(EventInputPool::new(1, 256));
            let seen = Rc::new(RefCell::new(None));
            let handler_seen = Rc::clone(&seen);
            registry
                .register_json::<InternalEmit<256>, _>(
                    "system",
                    move |_context, request, response: JsonWriter| {
                        let seen = Rc::clone(&handler_seen);
                        let pool = Rc::clone(&pool);
                        async move {
                            let (header, input) = EventInput::accept(request, &pool)?;
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
