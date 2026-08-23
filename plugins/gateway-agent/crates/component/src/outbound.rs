use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_agent_component::dto::{SessionIdDto, SessionPersistenceDto};
use barracuda_agent_component::new_session::{NewSession, NewSessionRequest};
use barracuda_agent_component::open_session::{
    OpenSession, OpenSessionRequest, OpenSessionResponse, SessionEventDto,
};
use barracuda_event_router::{RpcClient, RpcError, RpcResult, RpcStream};
use barracuda_message_gateway_component::gateway_send::{
    frames_from_gateway_send, GatewayOutboundMessage, GatewaySend,
};
use futures_lite::stream;
use gateway::MessageKind;

use crate::component::BridgeState;

/// Failure that ends the outbound pump and unloads the bridge.
#[derive(Debug, thiserror::Error)]
pub enum PumpError {
    /// The session event stream or a `gateway.send` call failed on the wire.
    #[error("bridge RPC transport failure: {0}")]
    Rpc(#[from] RpcError),
    /// The Agent runtime rejected creating the bridge's session.
    #[error("could not create the bridge Agent session")]
    SessionCreate,
}

/// Creates the persistent Agent session that backs the bridge.
pub(crate) async fn create_session(client: &RpcClient) -> Result<SessionIdDto, PumpError> {
    let request = NewSessionRequest {
        persistence: SessionPersistenceDto::Persistent,
    };
    match client.call::<NewSession>(request)?.await? {
        Ok(response) => Ok(response.view()?.session),
        Err(_error) => Err(PumpError::SessionCreate),
    }
}

/// Opens the bound Agent session and maps each turn event onto a `gateway.send`
/// call, tagging the message with a generic [`MessageKind`] role.
///
/// Runs for the Component's lifetime: after the session stream closes it stays
/// pending so the Component remains resident.
pub(crate) async fn pump_session_events(
    client: RpcClient,
    state: Rc<BridgeState>,
    session: SessionIdDto,
) -> Result<(), PumpError> {
    let request = OpenSessionRequest { session };
    let mut events = client.call::<OpenSession>(request)?;
    state.opened.set(true);

    let mut aggregator = Aggregator::default();
    while let Some(item) = events.next().await {
        let frame = match item? {
            Ok(frame) => frame,
            // A per-open error (e.g. the session was momentarily unavailable):
            // keep pumping rather than tearing down the Component.
            Err(_error) => continue,
        };
        let response: OpenSessionResponse = match serde_json::from_str(frame.view()?.json.as_str())
        {
            Ok(response) => response,
            Err(_error) => continue,
        };
        if let OpenSessionResponse::Event { event, .. } = response {
            for message in aggregator.absorb(event) {
                deliver(&client, &state, message).await?;
            }
        }
    }

    let () = core::future::pending().await;
    Ok(())
}

async fn deliver(
    client: &RpcClient,
    state: &Rc<BridgeState>,
    message: OutMessage,
) -> Result<(), PumpError> {
    let OutMessage { kind, text } = message;
    // Only the primary reply threads onto the inbound message.
    let reply_to = if matches!(kind, MessageKind::Reply) {
        state.latest_reply_to.borrow_mut().take()
    } else {
        None
    };
    let outbound = GatewayOutboundMessage {
        route: state.route.clone(),
        text,
        reply_to,
        kind,
    };
    let frames = match frames_from_gateway_send(&outbound) {
        Ok(frames) => frames,
        Err(_error) => return Ok(()),
    };
    let input = RpcStream::new(stream::iter(frames.into_iter().map(RpcResult::Ok)));
    // Business delivery errors are non-fatal to the pump; ignore the inner result.
    let _outcome = client.call::<GatewaySend>(input)?.await?;
    Ok(())
}

/// One outbound IM message derived from Agent session events.
struct OutMessage {
    kind: MessageKind,
    text: String,
}

/// Accumulates streamed reasoning and output text into whole IM messages.
#[derive(Default)]
struct Aggregator {
    reasoning: String,
    output: String,
}

impl Aggregator {
    fn absorb(&mut self, event: SessionEventDto) -> Vec<OutMessage> {
        match event {
            SessionEventDto::ReasoningDelta { text } => {
                self.reasoning.push_str(&text);
                Vec::new()
            }
            SessionEventDto::ReasoningEnded => self.flush_reasoning(),
            SessionEventDto::OutputDelta { text } | SessionEventDto::EffectOutputDelta { text } => {
                self.output.push_str(&text);
                Vec::new()
            }
            SessionEventDto::OutputEnded | SessionEventDto::EffectOutputEnded => {
                self.flush_output()
            }
            SessionEventDto::ToolResult { call, output } => {
                let status = if output.ok { "ok" } else { "failed" };
                message(MessageKind::Tool, format!("{}: {status}", call.name))
            }
            SessionEventDto::TurnError { message: text }
            | SessionEventDto::SessionError { message: text } => {
                message(MessageKind::Notice, format!("error: {text}"))
            }
            SessionEventDto::InputRequested { .. } => message(
                MessageKind::Notice,
                String::from("input required to continue"),
            ),
            SessionEventDto::TurnEnded { .. } => {
                let mut flushed = self.flush_reasoning();
                flushed.append(&mut self.flush_output());
                flushed
            }
            _ => Vec::new(),
        }
    }

    fn flush_reasoning(&mut self) -> Vec<OutMessage> {
        if self.reasoning.is_empty() {
            return Vec::new();
        }
        message(MessageKind::Reasoning, core::mem::take(&mut self.reasoning))
    }

    fn flush_output(&mut self) -> Vec<OutMessage> {
        if self.output.is_empty() {
            return Vec::new();
        }
        message(MessageKind::Reply, core::mem::take(&mut self.output))
    }
}

fn message(kind: MessageKind, text: String) -> Vec<OutMessage> {
    alloc::vec![OutMessage { kind, text }]
}
