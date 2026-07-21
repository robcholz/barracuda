//! Durable state projection assembled from BaseAgent and its context adapters.

use super::context_adapters::AgentMode;

/// The currently recoverable subset of one agent's runtime state.
///
/// Tool-call tracking will extend this value when the tool runner gains a
/// durable in-flight protocol. Runtime futures and adapter objects themselves
/// are deliberately never captured here.
pub(crate) struct AgentRecoverySnapshot {
    mode: AgentMode,
    loaded_tool_groups: Vec<String>,
}

impl AgentRecoverySnapshot {
    pub(crate) fn into_parts(self) -> (AgentMode, Vec<String>) {
        (self.mode, self.loaded_tool_groups)
    }
}

/// Sink context adapters use to contribute their durable projection.
///
/// BaseAgent drives this sink without importing any concrete adapter state.
pub(crate) struct AgentRecoverySnapshotBuilder {
    mode: AgentMode,
    mode_contributed: bool,
    loaded_tool_groups: Vec<String>,
}

impl AgentRecoverySnapshotBuilder {
    pub(crate) fn new(loaded_tool_groups: Vec<String>) -> Self {
        Self {
            // An Agent without a mode adapter has no mode-specific behavior;
            // Normal is therefore its complete semantic state, not a storage
            // fallback. The configured Factory always installs the adapter.
            mode: AgentMode::Normal,
            mode_contributed: false,
            loaded_tool_groups,
        }
    }

    pub(crate) fn set_mode(&mut self, mode: AgentMode) {
        debug_assert!(
            !self.mode_contributed,
            "multiple adapters contributed agent mode"
        );
        if !self.mode_contributed {
            self.mode = mode;
            self.mode_contributed = true;
        }
    }

    pub(crate) fn finish(self) -> AgentRecoverySnapshot {
        AgentRecoverySnapshot {
            mode: self.mode,
            loaded_tool_groups: self.loaded_tool_groups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRecoverySnapshotBuilder;
    use crate::agent::base_agent::ContextAdapter;
    use crate::agent::context_adapters::{AgentMode, AgentModeContextAdapter};

    #[test]
    fn mode_adapter_contributes_without_base_agent_interpreting_mode() {
        let (effects, _inbox) = crate::agent::effect::agent_effect_channel();
        let adapter = AgentModeContextAdapter::new(AgentMode::Plan, effects);
        let mut snapshot = AgentRecoverySnapshotBuilder::new(vec!["memory".to_owned()]);

        adapter.contribute_recovery(&mut snapshot);

        assert_eq!(
            snapshot.finish().into_parts(),
            (AgentMode::Plan, vec!["memory".to_owned()])
        );
    }
}
