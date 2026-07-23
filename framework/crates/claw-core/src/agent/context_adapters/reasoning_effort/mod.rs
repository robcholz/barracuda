//! Per-agent reasoning-effort context.

use async_channel::{Receiver, Sender};
use claw_context::{Block, BlockKind, ContextSink};
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::ContextAdapter;

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
    updates: Sender<ReasoningEffort>,
}

impl ReasoningEffortHandle {
    pub(crate) fn set(&self, effort: ReasoningEffort) {
        let result = self.updates.try_send(effort);
        debug_assert!(result.is_ok(), "live Agent reasoning inbox must be open");
    }
}

pub(crate) struct ReasoningEffortContextAdapter {
    effort: ReasoningEffort,
    updates: Receiver<ReasoningEffort>,
}

impl ReasoningEffortContextAdapter {
    /// Create the adapter and its owner-facing sending handle together.
    pub(crate) fn new(effort: ReasoningEffort) -> (Self, ReasoningEffortHandle) {
        let (updates, receiver) = async_channel::unbounded();
        (
            Self {
                effort,
                updates: receiver,
            },
            ReasoningEffortHandle { updates },
        )
    }
}

impl ContextAdapter for ReasoningEffortContextAdapter {
    fn contribute(&mut self, output: &mut ContextSink<'_>) {
        while let Ok(effort) = self.updates.try_recv() {
            self.effort = effort;
        }
        output.block(self.effort.context_block());
    }
}

#[cfg(test)]
mod tests {
    use claw_context::{BlockKind, Context};

    use super::{ReasoningEffort, ReasoningEffortContextAdapter};
    use crate::agent::base_agent::ContextAdapter;

    fn render(adapter: &mut ReasoningEffortContextAdapter, context: &mut Context) -> String {
        let history = {
            let mut sink = context.sink();
            adapter.contribute(&mut sink);
            sink.into_history()
        };
        context.request(&history).system().to_owned()
    }

    #[test]
    fn update_changes_only_this_adapter() {
        let (mut adapter, handle) = ReasoningEffortContextAdapter::new(ReasoningEffort::Low);
        let (mut other, _other_handle) = ReasoningEffortContextAdapter::new(ReasoningEffort::Low);
        let mut context = Context::new();

        let low = render(&mut adapter, &mut context);
        assert!(low.contains("Reasoning effort: low"));

        handle.set(ReasoningEffort::Ultra);
        let ultra = render(&mut adapter, &mut context);
        assert!(ultra.contains("Reasoning effort: ultra"));
        assert!(!ultra.contains("Reasoning effort: low"));

        let mut other_context = Context::new();
        assert!(render(&mut other, &mut other_context).contains("Reasoning effort: low"));
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
