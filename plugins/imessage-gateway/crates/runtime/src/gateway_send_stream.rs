use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use async_channel::{Receiver, Sender, TrySendError};
use barracuda_workflow_plugin::{Event, WorkflowService};
use futures_lite::stream;
use gateway::{
    MessageGateway, MessageTarget, SendStream, SendStreamEvent, SendStreamRequest, StreamError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::{map_gateway_error, valid_required, GatewayAccepted, GatewayOperationError};
use crate::route::GatewayRoute;
use crate::runtime::STREAM_WORKERS;

const EVENT_QUEUE_CAPACITY: usize = 16;

/// One semantic event supplied to an outbound Gateway stream.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewaySendStreamRequest {
    /// Destination provider and conversation.
    pub route: GatewayRoute,
    /// Optional provider message being replied to.
    pub reply_to: Option<String>,
    /// Agent session that owns this event stream.
    pub session: String,
    /// Agent-assigned event sequence.
    pub sequence: u64,
    /// Forward-compatible semantic event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Complete semantic payload.
    pub payload: Value,
}

/// Terminal Workflow Event for one accepted outbound semantic stream.
pub struct GatewaySendStreamFinished;

impl Event for GatewaySendStreamFinished {
    const ID: &'static str = "gateway.send_stream.finished";
}

type StreamItem = Result<SendStreamEvent, StreamError>;

struct EventSession {
    last_sequence: Cell<u64>,
    target: MessageTarget,
    reply_to: Option<String>,
    terminal_sequence: Rc<RefCell<Option<u64>>>,
    events: Sender<StreamItem>,
}

pub(crate) struct EventJob {
    session: String,
    terminal_sequence: Rc<RefCell<Option<u64>>>,
    target: MessageTarget,
    reply_to: Option<String>,
    events: Receiver<StreamItem>,
}

#[derive(Default)]
pub(crate) struct EventSessions {
    entries: RefCell<BTreeMap<String, Rc<EventSession>>>,
}

impl EventSessions {
    fn remove(&self, session: &str) {
        self.entries.borrow_mut().remove(session);
    }

    fn push(
        &self,
        request: &GatewaySendStreamRequest,
        target: &MessageTarget,
        event: SendStreamEvent,
        terminal: bool,
    ) -> Result<(), GatewayOperationError> {
        let mut entries = self.entries.borrow_mut();
        let session = entries
            .get(&request.session)
            .cloned()
            .ok_or(GatewayOperationError::UnknownStream)?;
        if request.sequence <= session.last_sequence.get() {
            return Err(GatewayOperationError::OutOfOrder);
        }
        if target != &session.target || request.reply_to != session.reply_to {
            return Err(GatewayOperationError::InvalidRequest);
        }
        let item = if request.event_type == "stream_error" {
            Err(stream_error(&request.payload)?)
        } else {
            Ok(event)
        };
        match session.events.try_send(item) {
            Ok(()) => {
                session.last_sequence.set(request.sequence);
                if terminal {
                    session.terminal_sequence.replace(Some(request.sequence));
                    entries.remove(&request.session);
                }
                Ok(())
            }
            Err(TrySendError::Full(_item)) => Err(GatewayOperationError::Busy),
            Err(TrySendError::Closed(_item)) => {
                entries.remove(&request.session);
                Err(GatewayOperationError::UnknownStream)
            }
        }
    }
}

pub(crate) fn accept_stream(
    sessions: &EventSessions,
    jobs: &Sender<EventJob>,
    request: GatewaySendStreamRequest,
) -> Result<GatewayAccepted, GatewayOperationError> {
    if !valid_stream_request(&request) {
        return Err(GatewayOperationError::InvalidRequest);
    }
    let target = target(&request.route);
    let payload = serde_json::to_string(&request.payload)
        .map_err(|_error| GatewayOperationError::InvalidRequest)?;
    let event = SendStreamEvent::new(
        request.session.clone(),
        request.sequence,
        request.event_type.clone(),
        payload,
    );
    if request.event_type == "turn_started" {
        start_stream(sessions, jobs, &request, target, event)?;
    } else if request.event_type == "stream_error"
        && !sessions.entries.borrow().contains_key(&request.session)
    {
        start_failed_stream(jobs, &request, target, stream_error(&request.payload)?)?;
    } else {
        let terminal = matches!(request.event_type.as_str(), "turn_ended" | "stream_error");
        sessions.push(&request, &target, event, terminal)?;
    }
    Ok(GatewayAccepted {
        accepted_sequence: request.sequence,
    })
}

fn valid_stream_request(request: &GatewaySendStreamRequest) -> bool {
    valid_session(&request.session)
        && valid_required(&request.event_type)
        && valid_required(&request.route.channel)
        && valid_required(&request.route.conversation_id)
        && request.payload.is_object()
}

fn valid_session(session: &str) -> bool {
    session.strip_prefix("session-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn target(route: &GatewayRoute) -> MessageTarget {
    let mut target = MessageTarget::new(route.channel.clone(), route.conversation_id.clone());
    target.thread_id = route.thread_id.clone();
    target
}

fn stream_error(payload: &Value) -> Result<StreamError, GatewayOperationError> {
    payload
        .get("error")
        .and_then(Value::as_str)
        .filter(|message| valid_required(message))
        .map(StreamError::failed)
        .ok_or(GatewayOperationError::InvalidRequest)
}

fn start_stream(
    sessions: &EventSessions,
    jobs: &Sender<EventJob>,
    request: &GatewaySendStreamRequest,
    target: MessageTarget,
    event: SendStreamEvent,
) -> Result<(), GatewayOperationError> {
    if sessions.entries.borrow().contains_key(&request.session) {
        return Err(GatewayOperationError::DuplicateStream);
    }
    if sessions.entries.borrow().len() >= STREAM_WORKERS {
        return Err(GatewayOperationError::Busy);
    }
    let (events, receiver) = async_channel::bounded(EVENT_QUEUE_CAPACITY);
    events
        .try_send(Ok(event))
        .map_err(|_error| GatewayOperationError::Busy)?;
    let terminal_sequence = Rc::new(RefCell::new(None));
    let job = EventJob {
        session: request.session.clone(),
        terminal_sequence: Rc::clone(&terminal_sequence),
        target: target.clone(),
        reply_to: request.reply_to.clone(),
        events: receiver,
    };
    jobs.try_send(job)
        .map_err(|_error| GatewayOperationError::Busy)?;
    sessions.entries.borrow_mut().insert(
        request.session.clone(),
        Rc::new(EventSession {
            last_sequence: Cell::new(request.sequence),
            target,
            reply_to: request.reply_to.clone(),
            terminal_sequence,
            events,
        }),
    );
    Ok(())
}

fn start_failed_stream(
    jobs: &Sender<EventJob>,
    request: &GatewaySendStreamRequest,
    target: MessageTarget,
    error: StreamError,
) -> Result<(), GatewayOperationError> {
    let (events, receiver) = async_channel::bounded(EVENT_QUEUE_CAPACITY);
    events
        .try_send(Err(error))
        .map_err(|_error| GatewayOperationError::Busy)?;
    drop(events);
    jobs.try_send(EventJob {
        session: request.session.clone(),
        terminal_sequence: Rc::new(RefCell::new(Some(request.sequence))),
        target,
        reply_to: request.reply_to.clone(),
        events: receiver,
    })
    .map_err(|_error| GatewayOperationError::Busy)
}

pub(crate) async fn deliver_event_stream(
    gateway: &MessageGateway,
    sessions: &EventSessions,
    workflow: &WorkflowService,
    job: EventJob,
) {
    let session = job.session;
    let terminal_sequence = Rc::clone(&job.terminal_sequence);
    let request = SendStreamRequest {
        target: job.target,
        events: event_stream(job.events),
        reply_to: job.reply_to,
    };
    let result = gateway.send_stream(request).await;
    sessions.remove(&session);
    let sequence = terminal_sequence.borrow().unwrap_or_default();
    let terminal = match result {
        Ok(receipt) if terminal_sequence.borrow().is_some() => StreamTerminalEvent {
            session,
            sequence,
            outcome: "completed",
            message_id: Some(receipt.message_id),
            error: None,
        },
        Ok(_receipt) => StreamTerminalEvent {
            session,
            sequence,
            outcome: "failed",
            message_id: None,
            error: Some(GatewayOperationError::Delivery),
        },
        Err(error) => StreamTerminalEvent {
            session,
            sequence,
            outcome: "failed",
            message_id: None,
            error: Some(map_gateway_error(&error)),
        },
    };
    emit_terminal::<GatewaySendStreamFinished, _>(workflow, &terminal);
}

fn event_stream(events: Receiver<StreamItem>) -> SendStream {
    Box::pin(stream::unfold(events, |events| async move {
        events.recv().await.ok().map(|item| (item, events))
    }))
}

#[derive(Serialize)]
struct StreamTerminalEvent {
    session: String,
    sequence: u64,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<GatewayOperationError>,
}

fn emit_terminal<EventType, Payload>(workflow: &WorkflowService, payload: &Payload)
where
    EventType: Event,
    Payload: Serialize,
{
    match serde_json::to_value(payload) {
        Ok(value) => {
            if let Err(error) = workflow.emit::<EventType>(value) {
                log::error!("IMessage Gateway failed to emit terminal Event: {error}");
            }
        }
        Err(error) => log::error!("IMessage Gateway failed to serialize terminal Event: {error}"),
    }
}
