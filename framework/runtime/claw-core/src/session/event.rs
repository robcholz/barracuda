//! The event vocabulary a session event stream yields.
//!
//! One open session has one long-lived stream. Appending a user message creates
//! a turn. Only the **root** agent is externally visible, and a root's
//! iterations are sequential, so content events carry no agent id: the
//! `iteration` id is emitted once (on
//! [`SessionEvent::IterationStarted`]) and the following content events belong
//! to it by position.
//!
//! See `.agents/design/sse.md` for the full model (ordering, SSE forward-compat).

use claw_utils::stream::StreamPart;
use serde::{Deserialize, Serialize};

use crate::agent::{AgentId, IterationId};
use claw_api::ToolCall;

crate::define_prefixed_id!(InputRequestId, "input-", "input request");
crate::define_prefixed_id!(TurnId, "turn-", "turn");

/// What caused a root-visible turn to start.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOrigin {
    /// A public caller appended a message.
    #[default]
    User,
    /// A detached subagent delivered its result to its parent.
    Subagent {
        /// The subagent whose result caused this turn.
        agent: AgentId,
    },
}

impl TurnOrigin {
    pub(crate) fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }
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

/// One item in a session's event stream.
///
/// Content variants ([`Reasoning`](Self::Reasoning), [`Output`](Self::Output),
/// [`ToolCalls`](Self::ToolCalls)) are mutually exclusive per event. Within one
/// iteration they form three explicitly closed streams in this order:
/// `Reasoning(Delta)* -> Reasoning(End) -> Output(Delta)* -> Output(End) ->
/// ToolCalls(Delta)* -> ToolCalls(End)`. Diagnostic usage follows when cache
/// profiling is enabled.
/// `Reasoning`/`Output` deltas are **append fragments** (streaming emits many,
/// non-streaming one holding the whole string). Each `ToolCalls` delta is one
/// complete [`ToolCall`], in call order. Every content stream emits exactly one
/// [`StreamPart::End`] per iteration, even when it emitted no deltas.
/// A task-ending Agent effect may additionally emit one synthesized
/// `Output(Delta) -> Output(End)` after `IterationEnded` and before `TurnEnded`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// A root-visible turn started.
    TurnStarted {
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
    /// A root LLM round started. Carries the only iteration id on the stream.
    IterationStarted {
        /// The iteration this bracket opens.
        iteration: IterationId,
    },
    /// Model thinking text, truncated to the configured limit.
    Reasoning(StreamPart<String>),
    /// Assistant-visible text: a plain-text answer or a `conversation_end`
    /// closing message. Never truncated.
    Output(StreamPart<String>),
    /// Complete tool calls requested by the model this iteration. Each delta is
    /// one call; `End` means no more calls will be emitted for the iteration.
    ToolCalls(StreamPart<ToolCall>),
    /// Provider token/cache counters for the completed LLM iteration.
    #[cfg(feature = "cache_profile")]
    Usage {
        /// Counters reported by the provider; individual fields may be absent.
        usage: claw_api::ApiUsage,
    },
    /// The current root iteration ended.
    IterationEnded,
    /// The turn ended.
    TurnEnded {
        /// The session-local turn this bracket closes.
        turn: TurnId,
    },
    /// This session work item failed.
    Error {
        /// A human-readable failure message.
        message: String,
    },
    /// The session was closed and no more events will be sent.
    Closed,
}
