//! The context-adapter port consumed by BaseAgent.
//!
//! BaseAgent owns this contract; concrete adapters implement it under
//! `agent/context_adapters`. Adapters pull the transcript view during
//! [`prepare`](ContextAdapter::prepare), project request context, and may expose
//! one adapter-owned tool group.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use claw_context::ContextSink;
use claw_tool::ToolGroup;
use serde_json::Value;

use super::AgentStateBuilder;

/// Turn-lifecycle events observed by context adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent) enum TurnLifecycle {
    /// The current turn ended and adapter-local transient state may be reset.
    Ended,
}

/// Future returned by [`ContextAdapter::prepare`].
pub(in crate::agent) type ContextAdapterFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// A pluggable source that contributes to the next LLM request and may provide
/// model-callable tools.
///
/// Owned by the agent (one `Box<dyn ContextAdapter>` per registration) and driven
/// from its single execution thread.
///
/// The agent does not decide whether a source is a system block, history message,
/// or ephemeral reminder; each adapter emits the correct item into the sink and
/// `claw-context` owns placement, ordering, and render caches.
pub(in crate::agent) trait ContextAdapter {
    /// Refresh any async state needed for the next contribution.
    ///
    /// Called at the beginning of an LLM iteration before
    /// [`contribute`](Self::contribute).
    /// The default is a no-op for purely synchronous projectors.
    fn prepare<'a>(&'a mut self, _history: &'a dyn History) -> ContextAdapterFuture<'a> {
        Box::pin(async {})
    }

    /// Project this source into the request context for the current iteration.
    fn contribute(&mut self, output: &mut ContextSink<'_>);

    /// The model-callable tools this adapter provides.
    ///
    /// Added into the agent's tool set when the adapter is registered. Tool names
    /// must be globally unique across the agent's tools (a clash is rejected at
    /// registration). The default provides no tools.
    fn tools(&self) -> Option<ToolGroup> {
        None
    }

    /// Observe a turn-lifecycle transition.
    fn on_turn_lifecycle(&mut self, _lifecycle: TurnLifecycle) {}

    /// Add this adapter's typed durable DTO to the complete Agent state.
    fn contribute_state(&self, _state: &mut AgentStateBuilder) {}
}

/// Read-only transcript view used while assembling the next request.
pub(in crate::agent) trait History {
    fn messages(&self) -> Arc<Value>;
    fn version(&self) -> u64;
}

/// Assistant message shape committed at a task boundary.
pub(in crate::agent) enum AssistantCommit<'a> {
    RawJson(&'a str),
    PlainText(&'a str),
}

/// Writable transcript boundary owned by BaseAgent.
pub(in crate::agent) trait Transcript: History {
    fn append_user(&self, text: &str, starts_task: bool);
    fn commit_assistant(&self, commit: AssistantCommit<'_>);
    fn append_patch(&self, patch: &Value);
    fn commit_ended(&self, final_message: &str);
    fn discard_open_turn(&self);
    fn as_history(&self) -> &dyn History;
}
