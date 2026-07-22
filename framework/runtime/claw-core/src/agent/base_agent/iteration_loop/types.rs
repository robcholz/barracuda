use claw_api::{ChatError, ChatStreamEvent, ToolCall};
use claw_tool::{ToolExecution, ToolSetError, ToolSetHandle};
use serde_json::Value;
use strum::IntoStaticStr;

use super::{InflightToolCall, IterationId, ToolCallId};

/// Errors from one [`super::IterationLoop::run`] step.
#[derive(Clone, Debug, IntoStaticStr, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IterationLoopError {
    #[strum(serialize = "missing_provider_tool_call_id")]
    #[error("LLM tool call is missing its provider id")]
    MissingProviderToolCallId,
    #[strum(serialize = "duplicate_provider_tool_call_id")]
    #[error("LLM returned duplicate provider tool call id {0}")]
    DuplicateProviderToolCallId(String),
    #[strum(serialize = "malformed_tool_call")]
    #[error("prepared tool call is no longer valid")]
    MalformedToolCall,
    #[strum(serialize = "incomplete_tool_batch")]
    #[error("tool batch ended before every tool call id produced a result")]
    IncompleteToolBatch,
    #[strum(serialize = "chat")]
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[strum(serialize = "tools")]
    #[error(transparent)]
    Tools(#[from] ToolSetError),
}

/// Inputs for exactly one streamed LLM call.
pub(crate) struct LlmStep<'a> {
    pub(crate) iteration_id: IterationId,
    pub(crate) system_prompt: &'a str,
    pub(crate) messages: &'a Value,
    /// Ephemeral trailing messages for this request only (never persisted),
    /// appended after `messages`. Empty when there is nothing to nudge.
    pub(crate) reminders: &'a [Value],
    /// The tool view for this step. It stays stable for the whole iteration.
    pub(crate) tools: &'a ToolSetHandle<'a>,
}

/// One owner-visible event from an iteration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IterationEvent {
    Started(IterationId),
    Llm(ChatStreamEvent),
    #[cfg(feature = "cache_profile")]
    Usage(claw_api::ApiUsage),
    /// The calls are now visible to the owner. They cannot execute until the
    /// owner polls the stream again.
    BeforeToolCalls(Vec<InflightToolCall>),
}

/// One internal item produced by an [`super::IterationLoop`].
///
/// Normal iteration completion is represented by the surrounding stream
/// returning `None`; only cancellation and interruption need explicit items
/// because they carry distinct control semantics for `BaseAgent`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IterationLoopEvent {
    Llm(ChatStreamEvent),
    /// The complete calls are now visible to the owner. Tool execution cannot
    /// start until the consumer polls the iteration stream again.
    BeforeToolCalls(Vec<InflightToolCall>),
    ApprovalRequired {
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    },
    ToolResult {
        tool_call_id: String,
        execution: ToolExecution,
    },
    Interrupted,
    Cancelled,
}
