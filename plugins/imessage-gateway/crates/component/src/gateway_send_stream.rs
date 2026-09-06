use alloc::{boxed::Box, collections::BTreeMap, rc::Rc, string::String};
use core::{
    cell::{Cell, RefCell},
    fmt,
};

use async_channel::{Receiver, Sender, TrySendError};
use barracuda_event_router::{
    json_schema, Event, EventEmitter, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema,
    JsonWriter, RpcError,
};
use futures_lite::stream;
use gateway::{MessageGateway, MessageTarget, SendStream, SendStreamEvent, SendStreamRequest};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::component::STREAM_WORKERS;
use crate::gateway_send::map_gateway_error;
use crate::json::{
    encoded_json_len, event_input_capacity, valid_required, write_encoded_json, write_json_string,
    AckResponse, EncodedJson, ErrorResponse, GatewayJsonError, FRAME_CAPACITY,
};

const EVENT_QUEUE_CAPACITY: usize = 2;

/// Feeds one complete semantic Agent event into an outbound Gateway stream.
pub struct GatewaySendStream;

impl JsonRpcSchema for GatewaySendStream {
    const ADDRESS: &'static str = "gateway.send_stream";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("send_stream", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("send_stream", response);
    const MAX_REQUEST_BYTES: usize = FRAME_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = 128;
}

/// Terminal outcome for one accepted outbound semantic event stream.
pub struct GatewaySendStreamFinished;

impl Event for GatewaySendStreamFinished {
    const ID: &'static str = "gateway.send_stream.finished";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteRequest<'a> {
    #[serde(borrow)]
    channel: &'a str,
    #[serde(borrow)]
    conversation_id: &'a str,
    #[serde(default, borrow)]
    thread_id: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamRequest<'a> {
    #[serde(borrow)]
    route: RouteRequest<'a>,
    #[serde(default, borrow)]
    reply_to: Option<&'a str>,
    #[serde(borrow)]
    session: &'a str,
    sequence: u64,
    #[serde(rename = "type", borrow)]
    event_type: &'a str,
    #[serde(borrow)]
    payload: &'a RawValue,
}

struct EventSession {
    last_sequence: Cell<u64>,
    target: MessageTarget,
    reply_to: Option<String>,
    terminal_sequence: Rc<RefCell<Option<u64>>>,
    events: Sender<SendStreamEvent>,
}

pub(crate) struct EventJob {
    session: String,
    terminal_sequence: Rc<RefCell<Option<u64>>>,
    target: MessageTarget,
    reply_to: Option<String>,
    events: Receiver<SendStreamEvent>,
}

#[derive(Default)]
pub(crate) struct EventSessions {
    entries: RefCell<BTreeMap<String, Rc<EventSession>>>,
}

impl EventSessions {
    pub(crate) fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    fn remove(&self, session: &str) {
        self.entries.borrow_mut().remove(session);
    }

    fn push(
        &self,
        session_id: &str,
        sequence: u64,
        target: &MessageTarget,
        reply_to: Option<&str>,
        event: SendStreamEvent,
        terminal: bool,
    ) -> Result<(), GatewayJsonError> {
        let mut entries = self.entries.borrow_mut();
        let session = entries
            .get(session_id)
            .cloned()
            .ok_or(GatewayJsonError::UnknownStream)?;
        if sequence <= session.last_sequence.get() {
            return Err(GatewayJsonError::OutOfOrder);
        }
        if target != &session.target || reply_to != session.reply_to.as_deref() {
            return Err(GatewayJsonError::InvalidRequest);
        }
        match session.events.try_send(event) {
            Ok(()) => {
                session.last_sequence.set(sequence);
                if terminal {
                    session.terminal_sequence.replace(Some(sequence));
                    entries.remove(session_id);
                }
                Ok(())
            }
            Err(TrySendError::Full(_event)) => Err(GatewayJsonError::Busy),
            Err(TrySendError::Closed(_event)) => {
                entries.remove(session_id);
                Err(GatewayJsonError::UnknownStream)
            }
        }
    }
}

/// Builds the JSON event handler for [`GatewaySendStream`].
pub(crate) fn gateway_send_stream_handler(
    sessions: Rc<EventSessions>,
    jobs: Sender<EventJob>,
) -> impl JsonHandler {
    move |_context, document: JsonRef, response: JsonWriter| {
        let sessions = Rc::clone(&sessions);
        let jobs = jobs.clone();
        async move {
            let request = document.deserialize::<StreamRequest<'_>>()?;
            if !valid_request(&request) {
                return response
                    .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                    .await;
            }

            let target = target(&request.route);
            let Some(event) = SendStreamEvent::inline(
                request.session,
                request.sequence,
                request.event_type,
                request.payload.get(),
            ) else {
                return response
                    .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                    .await;
            };

            let result = if request.event_type == "turn_started" {
                start_stream(
                    &sessions,
                    &jobs,
                    request.session,
                    request.sequence,
                    target,
                    request.reply_to,
                    event,
                )
            } else {
                sessions.push(
                    request.session,
                    request.sequence,
                    &target,
                    request.reply_to,
                    event,
                    request.event_type == "turn_ended",
                )
            };

            match result {
                Ok(()) => {
                    response
                        .write(&AckResponse {
                            accepted_sequence: request.sequence,
                        })
                        .await
                }
                Err(error) => response.write(&ErrorResponse(error)).await,
            }
        }
    }
}

fn valid_request(request: &StreamRequest<'_>) -> bool {
    valid_session(request.session)
        && valid_required(request.event_type)
        && valid_required(request.route.channel)
        && valid_required(request.route.conversation_id)
        && request.payload.get().starts_with('{')
}

fn valid_session(session: &str) -> bool {
    session.strip_prefix("session-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn target(route: &RouteRequest<'_>) -> MessageTarget {
    let mut target = MessageTarget::new(route.channel, route.conversation_id);
    target.thread_id = route.thread_id.map(String::from);
    target
}

fn start_stream(
    sessions: &EventSessions,
    jobs: &Sender<EventJob>,
    session_id: &str,
    sequence: u64,
    target: MessageTarget,
    reply_to: Option<&str>,
    event: SendStreamEvent,
) -> Result<(), GatewayJsonError> {
    if sessions.entries.borrow().contains_key(session_id) {
        return Err(GatewayJsonError::DuplicateStream);
    }
    if sessions.entries.borrow().len() >= STREAM_WORKERS {
        return Err(GatewayJsonError::Busy);
    }

    let (events, receiver) = async_channel::bounded(EVENT_QUEUE_CAPACITY);
    events
        .try_send(event)
        .map_err(|_error| GatewayJsonError::Busy)?;
    let terminal_sequence = Rc::new(RefCell::new(None));
    let reply_to = reply_to.map(String::from);
    let job = EventJob {
        session: String::from(session_id),
        terminal_sequence: Rc::clone(&terminal_sequence),
        target: target.clone(),
        reply_to: reply_to.clone(),
        events: receiver,
    };
    match jobs.try_send(job) {
        Ok(()) => {
            sessions.entries.borrow_mut().insert(
                String::from(session_id),
                Rc::new(EventSession {
                    last_sequence: Cell::new(sequence),
                    target,
                    reply_to,
                    terminal_sequence,
                    events,
                }),
            );
            Ok(())
        }
        Err(TrySendError::Full(_job)) => Err(GatewayJsonError::Busy),
        Err(TrySendError::Closed(_job)) => Err(GatewayJsonError::Busy),
    }
}

pub(crate) async fn deliver_event_stream<const M: usize>(
    gateway: &MessageGateway,
    sessions: &EventSessions,
    emitter: &EventEmitter<M>,
    job: EventJob,
) -> Result<(), barracuda_event_router::EmitError> {
    let session = job.session;
    let terminal_sequence = Rc::clone(&job.terminal_sequence);
    let events = event_stream(job.events);
    let request = SendStreamRequest {
        target: job.target,
        events,
        reply_to: job.reply_to,
    };
    let result = gateway.send_stream(request).await;
    sessions.remove(&session);
    let completed_sequence = *terminal_sequence.borrow();
    let sequence = completed_sequence.unwrap_or_default();
    let mut terminal = match (&result, completed_sequence) {
        (Ok(receipt), Some(_)) => TerminalEvent::completed(&session, sequence, &receipt.message_id),
        (Ok(_receipt), None) => {
            TerminalEvent::failed(&session, sequence, GatewayJsonError::Delivery)
        }
        (Err(error), _) => TerminalEvent::failed(&session, sequence, map_gateway_error(error)),
    };
    let event_input_bytes = event_input_capacity::<M>(GatewaySendStreamFinished::ID)?;
    if terminal
        .encoded_len()
        .map_or(true, |length| length > event_input_bytes)
    {
        terminal = TerminalEvent::failed(&session, sequence, GatewayJsonError::InvalidReceipt);
    }
    emitter.emit::<GatewaySendStreamFinished>(&terminal).await
}

fn event_stream(events: Receiver<SendStreamEvent>) -> SendStream {
    Box::pin(stream::unfold(events, |events| async move {
        events.recv().await.ok().map(|event| (Ok(event), events))
    }))
}

enum TerminalOutcome<'a> {
    Completed(&'a str),
    Failed(GatewayJsonError),
}

struct TerminalEvent<'a> {
    session: &'a str,
    sequence: u64,
    outcome: TerminalOutcome<'a>,
}

impl<'a> TerminalEvent<'a> {
    const fn completed(session: &'a str, sequence: u64, message_id: &'a str) -> Self {
        Self {
            session,
            sequence,
            outcome: TerminalOutcome::Completed(message_id),
        }
    }

    const fn failed(session: &'a str, sequence: u64, error: GatewayJsonError) -> Self {
        Self {
            session,
            sequence,
            outcome: TerminalOutcome::Failed(error),
        }
    }
}

impl EncodedJson for TerminalEvent<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"session\":")?;
        write_json_string(writer, self.session)?;
        write!(writer, ",\"sequence\":{},\"outcome\":", self.sequence)?;
        match self.outcome {
            TerminalOutcome::Completed(message_id) => {
                writer.write_str("\"completed\",\"message_id\":")?;
                write_json_string(writer, message_id)?;
            }
            TerminalOutcome::Failed(error) => {
                writer.write_str("\"failed\",\"error\":")?;
                write_json_string(writer, error.code())?;
            }
        }
        writer.write_char('}')
    }
}

impl JsonPayload for TerminalEvent<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{boxed::Box, rc::Rc, string::String};

    use barracuda_event_router::{RpcAddress, RpcError, RpcLaneStorage, RpcRegistry};
    use gateway::{MessageTarget, SendStreamEvent};

    use super::{
        gateway_send_stream_handler, EventSession, EventSessions, GatewaySendStream,
        EVENT_QUEUE_CAPACITY,
    };
    use crate::json::GatewayJsonError;

    fn event(sequence: u64) -> SendStreamEvent {
        SendStreamEvent::new("session-1", sequence, "output_delta", r#"{"text":"x"}"#)
    }

    #[test]
    fn text_session_enforces_order_route_and_bounded_backpressure() {
        let sessions = EventSessions::default();
        let target = MessageTarget::new("test", "chat");
        let (events, receiver) = async_channel::bounded(EVENT_QUEUE_CAPACITY);
        sessions.entries.borrow_mut().insert(
            String::from("session-1"),
            Rc::new(EventSession {
                last_sequence: core::cell::Cell::new(1),
                target: target.clone(),
                reply_to: None,
                terminal_sequence: Rc::new(core::cell::RefCell::new(None)),
                events,
            }),
        );

        assert_eq!(
            sessions.push("session-1", 1, &target, None, event(1), false),
            Err(GatewayJsonError::OutOfOrder)
        );
        assert_eq!(
            sessions.push(
                "session-1",
                2,
                &MessageTarget::new("other", "chat"),
                None,
                event(2),
                false,
            ),
            Err(GatewayJsonError::InvalidRequest)
        );
        assert_eq!(
            sessions.push("session-1", 2, &target, None, event(2), false),
            Ok(())
        );
        assert_eq!(
            sessions.push("session-1", 3, &target, None, event(3), false),
            Ok(())
        );
        assert_eq!(
            sessions.push("session-1", 4, &target, None, event(4), true),
            Err(GatewayJsonError::Busy)
        );
        assert!(receiver.try_recv().is_ok());
        assert_eq!(
            sessions.push("session-1", 4, &target, None, event(4), true),
            Ok(())
        );
    }

    #[test]
    fn complete_semantic_event_request_is_bounded_by_the_rpc_lane() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 512, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let (jobs, _receiver) = async_channel::bounded(1);
        let _registration = registry
            .register_json::<GatewaySendStream, _>(
                "*",
                gateway_send_stream_handler(Rc::new(EventSessions::default()), jobs),
            )
            .expect("register Gateway stream endpoint");
        let address = RpcAddress::try_from("gateway.send_stream").expect("valid address");
        let payload = "x".repeat(512);
        let request = alloc::format!(
            r#"{{"route":{{"channel":"test","conversation_id":"chat"}},"session":"session-1","sequence":1,"type":"turn_started","payload":{{"text":"{payload}"}}}}"#
        );

        assert!(matches!(
            registry.client().call_json(&address, &request),
            Err(RpcError::FrameTooLarge { capacity: 512, .. })
        ));
    }
}
