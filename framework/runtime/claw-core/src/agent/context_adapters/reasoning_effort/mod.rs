//! Per-agent reasoning-effort context.

use async_channel::{Receiver, Sender};
use claw_context::ContextSink;

use crate::agent::base_agent::ContextAdapter;
use crate::config::ReasoningEffort;

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
    use claw_context::Context;

    use super::ReasoningEffortContextAdapter;
    use crate::agent::base_agent::ContextAdapter;
    use crate::config::ReasoningEffort;

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
}
