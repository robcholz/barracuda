use std::collections::HashSet;

use claw_api::LlmResponse;
use serde_json::Value;
use strum::IntoStaticStr;

use claw_api::ChatError;
use claw_tool::{ToolSetError, ToolSetHandle};

use crate::protocol::IterationId;

/// Errors from one [`super::IterationLoop::run`] step.
#[derive(Clone, Debug, IntoStaticStr, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IterationLoopError {
    #[strum(serialize = "missing_assistant_message")]
    #[error("LLM tool-call response missing raw assistant message JSON")]
    MissingAssistantMessage,
    #[strum(serialize = "malformed_assistant_message")]
    #[error("LLM raw assistant message JSON is not valid JSON")]
    MalformedAssistantMessage,
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

/// Owned message batch appended by one completed step (assistant/tool round).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppendedMessages {
    messages: Vec<Value>,
}

impl AppendedMessages {
    pub(crate) fn empty() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub(super) fn push(&mut self, message: Value) {
        self.messages.push(message);
    }

    pub(crate) fn into_json_array(self) -> Value {
        Value::Array(self.messages)
    }

    pub(crate) fn into_complete_json_array(self) -> Option<Value> {
        let mut expected = Vec::new();
        let mut satisfied = HashSet::new();
        for message in &self.messages {
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        expected.push(id);
                    }
                }
            }
            if let Some(id) = message.get("tool_call_id").and_then(Value::as_str) {
                satisfied.insert(id);
            }
        }
        if expected.iter().any(|id| !satisfied.contains(id)) {
            return None;
        }
        Some(Value::Array(self.messages))
    }
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

/// Terminal outcome of one streamed LLM call.
#[derive(Clone, Debug)]
pub(crate) enum IterationOutcome {
    Response(LlmResponse),
    Tools(AppendedMessages),
    Interrupted,
    Cancelled(AppendedMessages),
}

/// One [`super::IterationLoop::run`] step: [`IterationOutcome`] or [`IterationLoopError`].
pub(crate) type IterationResult = Result<IterationOutcome, IterationLoopError>;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::AppendedMessages;

    #[test]
    fn appended_messages_can_only_materialize_as_an_array() {
        let mut messages = AppendedMessages::empty();
        messages.push(json!({ "role": "assistant", "content": "working" }));
        messages.push(json!({ "role": "tool", "content": "done" }));

        assert_eq!(
            messages.into_json_array(),
            json!([
                { "role": "assistant", "content": "working" },
                { "role": "tool", "content": "done" }
            ])
        );
    }
}
