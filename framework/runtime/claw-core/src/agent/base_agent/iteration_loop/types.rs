use claw_api::{ChatError, ToolCall};
use claw_tool::{ToolDetachHandle, ToolExecution, ToolSetError, ToolSetHandle};
use claw_utils::stream::StreamPart;
use serde_json::Value;
use strum::IntoStaticStr;

use super::{IterationId, ToolCallId};

/// Errors from one [`super::IterationLoop::run`] step.
#[derive(Clone, Debug, IntoStaticStr, PartialEq, Eq, thiserror::Error)]
pub enum IterationLoopError {
    #[strum(serialize = "missing_provider_tool_call_id")]
    #[error("LLM tool call is missing its provider id")]
    MissingProviderToolCallId,
    #[strum(serialize = "duplicate_provider_tool_call_id")]
    #[error("LLM returned duplicate provider tool call id {0}")]
    DuplicateProviderToolCallId(String),
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

/// One event from an iteration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IterationEvent {
    Reasoning(StreamPart<String>),
    Output(StreamPart<String>),
    /// BaseAgent records these calls before polling the iteration again and
    /// allowing tool execution to begin.
    BeforeToolCalls(Vec<ToolCall>),
    ToolResult(StreamPart<(ToolCall, ToolExecution)>),
}

/// One internal item produced by an [`super::IterationLoop`].
///
/// Normal iteration completion is represented by the surrounding stream
/// returning `None`; only cancellation and interruption need explicit items
/// because they carry distinct control semantics for `BaseAgent`.
pub(crate) enum IterationLoopEvent {
    Iteration(IterationEvent),
    Detached(ToolDetachHandle),
    ApprovalRequired {
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    },
    Interrupted,
    Cancelled,
}
