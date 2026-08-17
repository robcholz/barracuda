//! The context-provider port consumed by BaseAgent.
//!
//! BaseAgent owns this contract; concrete providers implement it under
//! `agent/context_providers`. Providers pull the transcript view during
//! [`prepare`](ContextProvider::prepare), project request context, and may expose
//! one provider-owned tool group.

use alloc::boxed::Box;

use core::error::Error;
use core::future::Future;
use core::pin::Pin;

use crate::agent::base_agent::AgentStorage;
use barracuda_agent_context::ContextSink;
use barracuda_agent_memory::Transcript;
use barracuda_agent_tool::ToolGroup;

/// Turn-lifecycle events observed by context providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent) enum TurnLifecycle {
    /// The current turn ended and provider-local transient state may be reset.
    Ended,
}

/// Result returned through the provider port.
pub(in crate::agent) type ContextProviderResult =
    Result<(), Box<dyn Error + Send + Sync + 'static>>;

/// Future returned by [`ContextProvider::prepare`].
pub(in crate::agent) type ContextProviderFuture<'a> =
    Pin<Box<dyn Future<Output = ContextProviderResult> + 'a>>;

/// A pluggable source that contributes to the next LLM request and may provide
/// model-callable tools.
///
/// Owned by the agent (one `Box<dyn ContextProvider>` per registration) and driven
/// from its single execution thread.
///
/// The agent does not decide whether a source is a system block, history message,
/// or ephemeral reminder; each provider emits the correct item into the sink and
/// `barracuda-agent-context` owns placement, ordering, and render caches.
pub(in crate::agent) trait ContextProvider {
    /// Stable identity used as this provider's durable storage namespace.
    fn id(&self) -> &'static str;

    /// Refresh any async state needed for the next contribution.
    ///
    /// Called at the beginning of an LLM iteration before
    /// [`contribute`](Self::contribute).
    /// The default is a no-op for purely synchronous projectors.
    fn prepare<'a>(
        &'a mut self,
        _storage: &'a AgentStorage,
        _transcript: &'a dyn Transcript,
    ) -> ContextProviderFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    /// Project this source into the request context for the current iteration.
    fn contribute(
        &mut self,
        _storage: &AgentStorage,
        _output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        Ok(())
    }

    /// The model-callable tools this provider provides.
    ///
    /// Added into the agent's tool set when the provider is registered. Tool names
    /// must be globally unique across the agent's tools (a clash is rejected at
    /// registration). The default provides no tools.
    fn tools(&self, _storage: &AgentStorage) -> Option<ToolGroup> {
        None
    }

    /// Observe a turn-lifecycle transition.
    fn on_turn_lifecycle(&mut self, _storage: &AgentStorage, _lifecycle: TurnLifecycle) {}
}
