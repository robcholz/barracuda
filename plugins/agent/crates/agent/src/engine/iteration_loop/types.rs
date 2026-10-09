use barracuda_agent_memory::ChatMessage;
use barracuda_agent_tool::{BackgroundToolPool, ToolOutput, ToolSetError, ToolSetHandle};
#[cfg(feature = "cache_profile")]
use barracuda_model_api::ProviderUsage;
use barracuda_model_api::{Error as ModelError, ToolCall};
use barracuda_runtime_utils::stream::StreamPart;
use strum::IntoStaticStr;

use super::{IterationId, ToolCallId};

/// Errors from one `IterationLoop::run` step.
#[derive(Debug, IntoStaticStr, thiserror::Error)]
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
    #[strum(serialize = "chat_init")]
    #[error("failed to initialize LLM chat: {0}")]
    ChatInit(#[source] ModelError),
    #[strum(serialize = "chat_stream")]
    #[error("LLM chat stream failed: {0}")]
    ChatStream(#[source] ModelError),
    #[strum(serialize = "tools")]
    #[error(transparent)]
    Tools(#[from] ToolSetError),
}

/// Inputs for exactly one streamed LLM call.
pub(crate) struct LlmStep<'a> {
    pub(crate) iteration_id: IterationId,
    pub(crate) system_prompt: &'a str,
    pub(crate) messages: &'a [ChatMessage],
    /// Ephemeral trailing messages for this request only (never persisted),
    /// appended after `messages`. Empty when there is nothing to nudge.
    pub(crate) reminders: &'a [ChatMessage],
    /// The tool view for this step. It stays stable for the whole iteration.
    pub(crate) tools: &'a ToolSetHandle<'a>,
    /// Owns the background Tool calls this step accepts.
    pub(crate) background: &'a BackgroundToolPool,
}

/// One event from an iteration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IterationEvent {
    Reasoning(StreamPart<String>),
    /// Provider signature over the reasoning, recorded with it in the transcript.
    ReasoningSignature(String),
    Output(StreamPart<String>),
    #[cfg(feature = "cache_profile")]
    Usage(ProviderUsage),
    /// AgentEngine records these calls before polling the iteration again and
    /// allowing tool execution to begin.
    BeforeToolCalls(Vec<ToolCall>),
    ToolResult(StreamPart<(ToolCall, ToolOutput)>),
}

/// One internal item produced by an [`super::IterationLoop`].
///
/// Normal iteration completion is represented by the surrounding stream
/// returning `None`; only cancellation and interruption need explicit items
/// because they carry distinct control semantics for `AgentEngine`.
pub(crate) enum IterationLoopEvent {
    Iteration(IterationEvent),
    ApprovalRequired {
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    },
    Interrupted,
    Cancelled,
}
use alloc::{string::String, vec::Vec};
