//! The output half of an open Session: typed events and stream failures.
//!
//! One open session has one long-lived stream. Appending a user message creates
//! a [`TurnEvent`] bracket, and each root Agent iteration is nested inside it as
//! an [`IterationEvent`] bracket. Only the root Agent is externally visible, so
//! content events need no Agent id.
//!
//! See `.agents/design/sse.md` for the full model (ordering, SSE forward-compat).

use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use claw_api::ToolCall;
use claw_tool::ToolOutput;
use claw_utils::stream::StreamPart;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use super::approval_resolver::ApprovalResolverError;
use super::control::SessionCommand;
use crate::agent::{AgentApprovalError, AgentCreateError, AgentError, IterationId};

crate::define_prefixed_id!(InputRequestId, "input-", "input request");
crate::define_prefixed_id!(TurnId, "turn-", "turn");

/// What caused a root-visible turn to start.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOrigin {
    /// A public caller appended a message.
    #[default]
    User,
    /// A detached tool delivered its result after the previous turn ended.
    ToolCall {
        /// The original model-requested call whose completion opened the turn.
        call: ToolCall,
    },
}

// The reasoning cap is a compile-time tier, not a runtime knob. Exactly one of
// the mutually-exclusive `reasoning_short` / `reasoning_medium` / `reasoning_long`
// Cargo features selects it; the default is `reasoning_short`. Reject zero or
// multiple so the cap is never ambiguous.
#[cfg(not(any(
    feature = "reasoning_short",
    feature = "reasoning_medium",
    feature = "reasoning_long",
)))]
compile_error!(
    "enable exactly one reasoning tier feature: `reasoning_short`, `reasoning_medium`, or `reasoning_long`"
);
#[cfg(any(
    all(feature = "reasoning_short", feature = "reasoning_medium"),
    all(feature = "reasoning_short", feature = "reasoning_long"),
    all(feature = "reasoning_medium", feature = "reasoning_long"),
))]
compile_error!(
    "enable only one reasoning tier feature: `reasoning_short`, `reasoning_medium`, or `reasoning_long`"
);

/// Semantic input the active turn needs from its caller.
///
/// Callers choose how to present this request. A chat adapter may render it as
/// an ordinary assistant message, while a GUI may use dedicated controls.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputRequestKind {
    /// A tool action is waiting for human permission.
    PermissionApproval {
        /// The complete model-requested call awaiting authorization.
        tool_call: ToolCall,
        /// Policy-provided reason why this call requires approval.
        reason: String,
    },
}

/// One item in a Session's event stream.
#[derive(Debug)]
pub enum SessionEvent {
    /// An event scoped to the active turn.
    Turn(TurnEvent),
    /// A recoverable Session-scope problem.
    Error(SessionEventError),
    /// The Session was closed and no more events will be sent.
    Closed(SessionCloseReason),
}

/// An event scoped to one root-visible turn.
#[derive(Debug)]
pub enum TurnEvent {
    /// The turn started.
    Started {
        /// The session-local turn this bracket opens.
        turn: TurnId,
        /// Why the runtime opened this turn.
        origin: TurnOrigin,
    },
    /// The current turn is paused until the caller responds to this request.
    InputRequested {
        /// Session-local request id required by `SessionControl::respond`.
        request: InputRequestId,
        /// What input is required. Presentation remains caller-owned.
        kind: InputRequestKind,
    },
    /// An event scoped to the active root Agent iteration.
    Iteration(IterationEvent),
    /// Assistant-visible output synthesized after an iteration has ended.
    Output(StreamPart<String>),
    /// A recoverable problem scoped to this turn.
    Error(TurnEventError),
    /// The turn ended.
    Ended {
        /// The session-local turn this bracket closes.
        turn: TurnId,
    },
}

/// An event scoped to one root Agent iteration.
///
/// The three content streams are emitted in this order:
/// `Reasoning(Delta)* -> Reasoning(End) -> Output(Delta)* -> Output(End) ->
/// ToolResult(Delta)* -> ToolResult(End)`. Every content stream emits exactly one
/// [`StreamPart::End`], including streams with no deltas.
#[derive(Debug)]
pub enum IterationEvent {
    /// The iteration started. Carries its only iteration id.
    Started {
        /// The iteration this bracket opens.
        iteration: IterationId,
    },
    /// Model thinking text. Deltas are append fragments.
    Reasoning(StreamPart<String>),
    /// Assistant-visible model text. Deltas are append fragments.
    Output(StreamPart<String>),
    /// Completed tool executions. Each delta contains the original request and
    /// its result; `End` means no more results will be emitted this iteration.
    ToolResult(StreamPart<(ToolCall, ToolOutput)>),
    /// Provider token/cache counters for the completed LLM iteration.
    #[cfg(feature = "cache_profile")]
    Usage {
        /// Counters reported by the provider; individual fields may be absent.
        usage: claw_api::ApiUsage,
    },
    /// The iteration ended.
    Ended,
}

/// A recoverable Session-scope problem.
#[derive(Debug, thiserror::Error)]
pub enum SessionEventError {
    /// Deleting the Session's root Agent failed; the Session remains registered.
    #[error("session deletion failed: {source}")]
    DeleteFailed {
        /// The typed lower-layer failure.
        #[source]
        source: AgentCreateError,
    },
}

/// A recoverable problem reported inside an active [`TurnEvent`] bracket.
#[derive(Debug, thiserror::Error)]
pub enum TurnEventError {
    /// The turn's Agent execution failed. The Session remains open.
    #[error(transparent)]
    Execution(#[from] SessionTurnError),
    /// Resolving one caller response failed.
    #[error("input request {request} could not be resolved: {source}")]
    InputResolutionFailed {
        /// The response request that failed.
        request: InputRequestId,
        /// The typed lower-layer failure.
        #[source]
        source: SessionInputError,
    },
}

/// A typed lower-layer failure that ended one turn without invalidating the
/// Session stream.
#[derive(Debug, thiserror::Error)]
pub enum SessionTurnError {
    /// The root Agent could not be constructed or restored.
    #[error(transparent)]
    AgentCreate(#[from] AgentCreateError),
    /// The active Agent turn failed.
    #[error(transparent)]
    Agent(#[from] AgentError),
}

/// A typed lower-layer failure while resolving caller input.
#[derive(Debug, thiserror::Error)]
pub enum SessionInputError {
    /// The resolver could not interpret the caller's response.
    #[error(transparent)]
    Resolver(#[from] ApprovalResolverError),
    /// The resolved decision could not be delivered to the parked Agent.
    #[error(transparent)]
    AgentApproval(#[from] AgentApprovalError),
}

/// Why a Session event stream ended normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCloseReason {
    /// The caller explicitly closed the open Session lease.
    Requested,
    /// The Session was deleted.
    Deleted,
    /// The owning runtime shut down normally.
    RuntimeShutdown,
}

/// An unrecoverable failure yielded by a [`SessionStream`].
///
/// Every `Err(SessionError)` is terminal and is followed by `None`.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The runtime worker disappeared before the Session closed normally.
    #[error("agent runtime stopped before the session stream closed")]
    RuntimeStopped,
}

/// The read-only output half of an open Session.
///
/// The parallel [`SessionControl`](super::SessionControl) owns command ingress.
/// Dropping this stream closes their shared lease.
pub struct SessionStream {
    lease: u64,
    commands: Sender<SessionCommand>,
    events: Pin<Box<Receiver<SessionEvent>>>,
    terminated: bool,
}

impl SessionStream {
    pub(super) fn new(
        lease: u64,
        commands: Sender<SessionCommand>,
        events: Receiver<SessionEvent>,
    ) -> Self {
        Self {
            lease,
            commands,
            events: Box::pin(events),
            terminated: false,
        }
    }
}

impl Stream for SessionStream {
    type Item = Result<SessionEvent, SessionError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }
        match self.events.as_mut().poll_next(context) {
            Poll::Ready(Some(event)) => {
                if matches!(&event, SessionEvent::Closed(_)) {
                    self.terminated = true;
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(None) => {
                self.terminated = true;
                Poll::Ready(Some(Err(SessionError::RuntimeStopped)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for SessionStream {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        let (ack, _result) = async_channel::bounded(1);
        let _ = self.commands.try_send(SessionCommand::Close {
            lease: self.lease,
            ack,
        });
    }
}
