//! Per-agent reasoning-effort context.

use alloc::sync::Arc;

use crate::agent::base_agent::AgentStorage;
use barracuda_agent_context::{Block, BlockKind, ContextSink};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::{ContextProvider, ContextProviderResult};

const LOW_PROMPT: &str = prompt!("effort/low.md");
const MEDIUM_PROMPT: &str = prompt!("effort/medium.md");
const HIGH_PROMPT: &str = prompt!("effort/high.md");
const ULTRA_PROMPT: &str = prompt!("effort/ultra.md");

/// How deliberately an agent should reason about and orchestrate work.
///
/// Higher tiers prompt more decomposition, delegation, and verification.
/// Updates take effect at the Agent's next LLM iteration.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Take the shortest sound path and avoid delegation by default.
    Low,
    /// Use necessary steps and delegate only clearly separable work. The default.
    #[default]
    Medium,
    /// Deliberately decompose, delegate, and verify non-trivial work.
    High,
    /// Use multi-agent execution and independent verification when appropriate.
    Ultra,
}

impl ReasoningEffort {
    fn context_block(self) -> Block<'static> {
        let content = match self {
            Self::Low => LOW_PROMPT,
            Self::Medium => MEDIUM_PROMPT,
            Self::High => HIGH_PROMPT,
            Self::Ultra => ULTRA_PROMPT,
        };
        Block::new(BlockKind::ReasoningEffort, content)
    }
}

/// Sending endpoint retained by the Agent's logical owner.
pub(crate) struct ReasoningEffortHandle {
    updates: Arc<Signal<NoopRawMutex, ReasoningEffort>>,
}

impl ReasoningEffortHandle {
    pub(crate) fn set(&self, effort: ReasoningEffort) {
        self.updates.signal(effort);
    }
}

pub(crate) struct ReasoningEffortContextProvider {
    effort: ReasoningEffort,
    updates: Arc<Signal<NoopRawMutex, ReasoningEffort>>,
}

impl ReasoningEffortContextProvider {
    /// Create the provider and its owner-facing sending handle together.
    pub(crate) fn new(effort: ReasoningEffort) -> (Self, ReasoningEffortHandle) {
        let updates = Arc::new(Signal::new());
        (
            Self {
                effort,
                updates: Arc::clone(&updates),
            },
            ReasoningEffortHandle { updates },
        )
    }
}

impl ContextProvider for ReasoningEffortContextProvider {
    fn id(&self) -> &'static str {
        "reasoning_effort"
    }

    fn contribute(
        &mut self,
        _storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        if let Some(effort) = self.updates.try_take() {
            self.effort = effort;
        }
        output.block(self.effort.context_block());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use barracuda_agent_context::{BlockKind, Context};
    use barracuda_agent_persistence::DurableState;

    use super::{ReasoningEffort, ReasoningEffortContextProvider};
    use crate::agent::base_agent::{AgentStorage, ContextProvider};
    use crate::agent::{AgentKind, BaseAgentState};

    fn render(provider: &mut ReasoningEffortContextProvider, context: &mut Context) -> String {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        let storage = AgentStorage::new(&state, provider.id());
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(&storage, &mut sink).is_ok());
            sink.into_history()
        };
        context.request(&history).system().to_owned()
    }

    #[test]
    fn update_changes_only_this_provider() {
        let (mut provider, handle) = ReasoningEffortContextProvider::new(ReasoningEffort::Low);
        let (mut other, _other_handle) = ReasoningEffortContextProvider::new(ReasoningEffort::Low);
        let mut context = Context::new();

        let low = render(&mut provider, &mut context);
        assert!(low.contains("Reasoning effort: low"));

        handle.set(ReasoningEffort::Ultra);
        let ultra = render(&mut provider, &mut context);
        assert!(ultra.contains("Reasoning effort: ultra"));
        assert!(!ultra.contains("Reasoning effort: low"));

        let mut other_context = Context::new();
        assert!(render(&mut other, &mut other_context).contains("Reasoning effort: low"));
    }

    #[test]
    fn pending_updates_keep_only_the_latest_effort() {
        let (mut provider, handle) = ReasoningEffortContextProvider::new(ReasoningEffort::Low);
        handle.set(ReasoningEffort::High);
        handle.set(ReasoningEffort::Ultra);

        let mut context = Context::new();
        let rendered = render(&mut provider, &mut context);
        assert!(rendered.contains("Reasoning effort: ultra"));
        assert!(!rendered.contains("Reasoning effort: high"));
    }

    #[test]
    fn every_effort_has_a_reasoning_effort_context_block() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Ultra,
        ] {
            let block = effort.context_block();
            assert_eq!(block.kind, BlockKind::ReasoningEffort);
            assert!(!block.content.trim().is_empty());
        }
    }
}
