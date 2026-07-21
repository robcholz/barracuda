//! Transcript ports consumed by BaseAgent and its context adapters.

use std::sync::Arc;

use serde_json::Value;

/// Read-only transcript view used while assembling the next request.
pub(in crate::agent) trait History {
    /// Current transcript as a shared JSON array of chat messages.
    fn messages(&self) -> Arc<Value>;

    /// Monotonic counter advanced whenever [`messages`](Self::messages) changes.
    fn version(&self) -> u64;
}

/// Assistant message shape committed at a task boundary.
pub(in crate::agent) enum AssistantCommit<'a> {
    /// Backend-shaped assistant message JSON returned by the LLM.
    RawJson(&'a str),
    /// Plain assistant text that the store wraps as an assistant message.
    PlainText(&'a str),
}

/// Writable transcript boundary owned by BaseAgent.
pub(in crate::agent) trait Transcript: History {
    /// Append user input, closing a previous open turn when `starts_task`.
    fn append_user(&self, text: &str, starts_task: bool);

    /// Commit the model's answer and close the open turn.
    fn commit_assistant(&self, commit: AssistantCommit<'_>);

    /// Append one materialized assistant/tool patch without closing the turn.
    fn append_patch(&self, patch: &Value);

    /// Commit an adapter/tool-directed closing message.
    fn commit_ended(&self, final_message: &str);

    /// Discard an unfinished turn during hard cancellation.
    fn discard_open_turn(&self);

    /// Borrow the read-only view without relying on trait upcasting.
    fn as_history(&self) -> &dyn History;
}
