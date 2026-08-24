use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_component::dto::{FixedStr, SessionIdDto, SessionPersistenceDto};
use barracuda_agent_component::new_session::{NewSession, NewSessionRequest};
use barracuda_agent_component::open_session::{
    OpenSession, OpenSessionError, OpenSessionRequest, OpenSessionResponse,
    OpenSessionResponseDecoder, OpenSessionResponseFrame, SessionEventDto,
};
use barracuda_agent_component::session::append::{Append, AppendRequestFrame};
use barracuda_event_router::{
    RpcContext, RpcFrame, RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming,
};
use barracuda_imessage_gateway_component::gateway_message_received::{
    gateway_event_from_frames, GatewayEventFrame,
};
use barracuda_imessage_gateway_component::gateway_send_stream::{
    frames_from_gateway_send_stream, frames_from_gateway_stream_frame, GatewayOutboundStream,
    GatewaySendStreamRequestFrame,
};
use barracuda_imessage_gateway_component::route::GatewayRoute;
use barracuda_imessage_gateway_component::wire::GatewayWireError;
use futures_lite::stream;
use futures_lite::Stream;
use gateway::{SendStreamField, SendStreamFrame, StreamBoundary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

type AgentEventStream =
    RpcStream<Result<RpcFrame<OpenSessionResponseFrame>, RpcFrame<OpenSessionError>>>;

/// Stateful Agent session associated with one Gateway conversation route.
struct Conversation {
    session: SessionIdDto,
    events: RefCell<AgentEventStream>,
    busy: Cell<bool>,
}

/// Per-component conversation/session registry.
#[derive(Default)]
pub(crate) struct RespondState {
    conversations: RefCell<BTreeMap<GatewayRoute, Rc<Conversation>>>,
    creating: RefCell<BTreeSet<GatewayRoute>>,
}

impl RespondState {
    pub(crate) fn clear(&self) {
        self.conversations.borrow_mut().clear();
        self.creating.borrow_mut().clear();
    }
}

/// Business failure returned by `gateway_agent.respond`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub enum GatewayAgentRespondError {
    /// The inbound Gateway Event did not follow its typed frame protocol.
    InvalidMessage,
    /// Another turn for the same conversation is still streaming.
    ConversationBusy,
    /// The Agent session could not be created or opened.
    SessionUnavailable,
    /// The Agent rejected the inbound user message.
    AppendFailed,
    /// An Agent event could not be decoded or mapped.
    InvalidEvent,
}

/// Stateful streaming mapper from one inbound Gateway Event to Gateway frames.
pub struct GatewayAgentRespond;

impl RpcMethod for GatewayAgentRespond {
    const ADDRESS: &'static str = "gateway_agent.respond";
    type Request = GatewayEventFrame;
    type Response = GatewaySendStreamRequestFrame;
    type Error = GatewayAgentRespondError;
    type Input = Streaming;
    type Output = Streaming;
}

/// Builds the reusable `gateway_agent.respond` handler.
pub(crate) fn gateway_agent_respond_handler(
    state: Rc<RespondState>,
) -> impl RpcHandler<GatewayAgentRespond> {
    move |context: RpcContext, frames: RpcStream<RpcFrame<GatewayEventFrame>>| {
        let state = Rc::clone(&state);
        async move {
            let inbound = match collect_inbound(frames).await? {
                Ok(inbound) => inbound,
                Err(error) => return Ok(error_stream(error)),
            };
            let conversation =
                match get_or_create_conversation(&context, &state, inbound.route.clone()).await? {
                    Ok(conversation) => conversation,
                    Err(error) => return Ok(error_stream(error)),
                };
            if conversation.busy.replace(true) {
                return Ok(error_stream(GatewayAgentRespondError::ConversationBusy));
            }
            if let Err(error) = append_message(&context, &conversation, &inbound.text).await? {
                conversation.busy.set(false);
                return Ok(error_stream(error));
            }

            let metadata = GatewayOutboundStream {
                route: inbound.route,
                reply_to: Some(inbound.message_id),
                frames: Vec::new(),
            };
            let pending = match frames_from_gateway_send_stream(&metadata) {
                Ok(frames) => frames.into(),
                Err(_error) => {
                    conversation.busy.set(false);
                    return Ok(error_stream(GatewayAgentRespondError::InvalidMessage));
                }
            };
            Ok(RpcStream::new(TurnStream::new(conversation, pending)))
        }
    }
}

fn error_stream(
    error: GatewayAgentRespondError,
) -> RpcStream<Result<GatewaySendStreamRequestFrame, GatewayAgentRespondError>> {
    RpcStream::new(stream::once(Ok(Err(error))))
}

async fn collect_inbound(
    mut frames: RpcStream<RpcFrame<GatewayEventFrame>>,
) -> RpcResult<
    Result<
        barracuda_imessage_gateway_component::gateway_message_received::GatewayInboundMessage,
        GatewayAgentRespondError,
    >,
> {
    let mut collected = Vec::new();
    while let Some(frame) = frames.next().await {
        collected.push(*frame?.view()?);
    }
    Ok(gateway_event_from_frames(collected)
        .map_err(|_error| GatewayAgentRespondError::InvalidMessage))
}

async fn get_or_create_conversation(
    context: &RpcContext,
    state: &Rc<RespondState>,
    route: GatewayRoute,
) -> RpcResult<Result<Rc<Conversation>, GatewayAgentRespondError>> {
    if let Some(conversation) = state.conversations.borrow().get(&route).cloned() {
        return Ok(Ok(conversation));
    }
    if !state.creating.borrow_mut().insert(route.clone()) {
        return Ok(Err(GatewayAgentRespondError::ConversationBusy));
    }

    let created = create_conversation(context).await;
    state.creating.borrow_mut().remove(&route);
    let conversation = match created? {
        Ok(conversation) => Rc::new(conversation),
        Err(error) => return Ok(Err(error)),
    };
    let conversation = state
        .conversations
        .borrow_mut()
        .entry(route)
        .or_insert_with(|| Rc::clone(&conversation))
        .clone();
    Ok(Ok(conversation))
}

async fn create_conversation(
    context: &RpcContext,
) -> RpcResult<Result<Conversation, GatewayAgentRespondError>> {
    let new = NewSessionRequest {
        persistence: SessionPersistenceDto::Persistent,
    };
    let session = match context.client().call::<NewSession>(new)?.await? {
        Ok(frame) => frame.view()?.session,
        Err(_error) => return Ok(Err(GatewayAgentRespondError::SessionUnavailable)),
    };
    let mut events = context
        .client()
        .call::<OpenSession>(OpenSessionRequest { session })?;
    let mut decoder = OpenSessionResponseDecoder::new();
    loop {
        let Some(item) = events.next().await else {
            return Ok(Err(GatewayAgentRespondError::SessionUnavailable));
        };
        let frame = match item? {
            Ok(frame) => *frame.view()?,
            Err(_error) => return Ok(Err(GatewayAgentRespondError::SessionUnavailable)),
        };
        match decoder.push(frame) {
            Ok(Some(OpenSessionResponse::Opened { .. })) => break,
            Ok(Some(OpenSessionResponse::Event { .. })) | Ok(None) => {}
            Err(_error) => return Ok(Err(GatewayAgentRespondError::InvalidEvent)),
        }
    }
    Ok(Ok(Conversation {
        session,
        events: RefCell::new(events),
        busy: Cell::new(false),
    }))
}

async fn append_message(
    context: &RpcContext,
    conversation: &Conversation,
    text: &str,
) -> RpcResult<Result<(), GatewayAgentRespondError>> {
    let Some(text) = fit_message_text(text) else {
        return Ok(Err(GatewayAgentRespondError::InvalidMessage));
    };
    let input = RpcStream::new(stream::once(Ok(AppendRequestFrame {
        session: conversation.session,
        text,
    })));
    match context.client().call::<Append>(input)?.await? {
        Ok(_frame) => Ok(Ok(())),
        Err(_error) => Ok(Err(GatewayAgentRespondError::AppendFailed)),
    }
}

fn fit_message_text<const N: usize>(text: &str) -> Option<FixedStr<N>> {
    FixedStr::new(text).ok()
}

/// Maps one logical Agent event to zero or more Gateway RPC frames.
///
/// # Errors
///
/// Returns a wire error if serialized event content contains an embedded NUL.
pub fn gateway_frames_from_session_event(
    event: &SessionEventDto,
) -> Result<Vec<GatewaySendStreamRequestFrame>, GatewayWireError> {
    let logical = logical_gateway_frames(event)?;
    let mut wire = Vec::new();
    for frame in logical {
        wire.extend(frames_from_gateway_stream_frame(&frame)?);
    }
    Ok(wire)
}

fn logical_gateway_frames(
    event: &SessionEventDto,
) -> Result<Vec<SendStreamFrame>, GatewayWireError> {
    use SendStreamField as Field;
    use StreamBoundary::{Complete, More};

    let frame = match event {
        SessionEventDto::ReasoningDelta { text } => {
            alloc::vec![SendStreamFrame::new(Field::Reasoning, More, text)]
        }
        SessionEventDto::ReasoningEnded => {
            alloc::vec![SendStreamFrame::new(Field::Reasoning, Complete, "")]
        }
        SessionEventDto::OutputDelta { text } => {
            alloc::vec![SendStreamFrame::new(Field::Text, More, text)]
        }
        SessionEventDto::OutputEnded => {
            alloc::vec![SendStreamFrame::new(Field::Text, Complete, "")]
        }
        SessionEventDto::EffectOutputDelta { text } => {
            alloc::vec![SendStreamFrame::new(Field::EffectResult, More, text)]
        }
        SessionEventDto::EffectOutputEnded => {
            alloc::vec![SendStreamFrame::new(Field::EffectResult, Complete, "")]
        }
        SessionEventDto::ToolResult { call, output } => alloc::vec![
            SendStreamFrame::new(Field::ToolResultStart, Complete, ""),
            SendStreamFrame::new(Field::ToolCallId, Complete, &call.id),
            SendStreamFrame::new(Field::ToolName, Complete, &call.name),
            SendStreamFrame::new(Field::ToolArguments, Complete, &call.arguments_json),
            SendStreamFrame::new(Field::ToolOutput, Complete, &output.content),
            SendStreamFrame::new(
                if output.ok {
                    Field::ToolSucceeded
                } else {
                    Field::ToolFailed
                },
                Complete,
                "",
            ),
            SendStreamFrame::new(Field::ToolResultEnd, Complete, ""),
        ],
        SessionEventDto::TurnError { message } | SessionEventDto::SessionError { message } => {
            alloc::vec![SendStreamFrame::new(Field::Notice, Complete, message)]
        }
        other => {
            let json =
                serde_json::to_string(other).map_err(|_error| GatewayWireError::InvalidRequest)?;
            alloc::vec![SendStreamFrame::new(Field::Event, Complete, json)]
        }
    };
    Ok(frame)
}

struct TurnStream {
    conversation: Rc<Conversation>,
    decoder: OpenSessionResponseDecoder,
    pending: VecDeque<GatewaySendStreamRequestFrame>,
    terminal: bool,
}

impl TurnStream {
    fn new(
        conversation: Rc<Conversation>,
        pending: VecDeque<GatewaySendStreamRequestFrame>,
    ) -> Self {
        Self {
            conversation,
            decoder: OpenSessionResponseDecoder::new(),
            pending,
            terminal: false,
        }
    }

    fn finish(&mut self) {
        self.terminal = true;
        self.conversation.busy.set(false);
    }
}

impl Drop for TurnStream {
    fn drop(&mut self) {
        self.conversation.busy.set(false);
    }
}

impl Stream for TurnStream {
    type Item = RpcResult<Result<GatewaySendStreamRequestFrame, GatewayAgentRespondError>>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(Ok(frame))));
            }
            if self.terminal {
                return Poll::Ready(None);
            }

            let polled = {
                let mut events = self.conversation.events.borrow_mut();
                Pin::new(&mut *events).poll_next(context)
            };
            let item = match polled {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.finish();
                    return Poll::Ready(Some(Ok(Err(
                        GatewayAgentRespondError::SessionUnavailable,
                    ))));
                }
                Poll::Ready(Some(item)) => item,
            };
            let outcome = match item {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.finish();
                    return Poll::Ready(Some(Err(error)));
                }
            };
            let frame = match outcome {
                Ok(frame) => match frame.view() {
                    Ok(frame) => *frame,
                    Err(error) => {
                        self.finish();
                        return Poll::Ready(Some(Err(error)));
                    }
                },
                Err(_error) => {
                    self.finish();
                    return Poll::Ready(Some(Ok(Err(GatewayAgentRespondError::InvalidEvent))));
                }
            };
            let response = match self.decoder.push(frame) {
                Ok(Some(response)) => response,
                Ok(None) => continue,
                Err(_error) => {
                    self.finish();
                    return Poll::Ready(Some(Ok(Err(GatewayAgentRespondError::InvalidEvent))));
                }
            };
            let OpenSessionResponse::Event { event, .. } = response else {
                self.finish();
                return Poll::Ready(Some(Ok(Err(GatewayAgentRespondError::InvalidEvent))));
            };
            let turn_ended = matches!(event, SessionEventDto::TurnEnded { .. });
            match gateway_frames_from_session_event(&event) {
                Ok(frames) => self.pending.extend(frames),
                Err(_error) => {
                    self.finish();
                    return Poll::Ready(Some(Ok(Err(GatewayAgentRespondError::InvalidEvent))));
                }
            }
            if turn_ended {
                self.terminal = true;
                self.conversation.busy.set(false);
            }
        }
    }
}
