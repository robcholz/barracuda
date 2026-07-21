//! Agent-mode adapter: state, context projection, and lifecycle behavior.
//!
//! Mode is adapter-local state. BaseAgent only drives the generic adapter,
//! effect, and task-lifecycle protocols.

use std::sync::{Arc, Mutex, MutexGuard};

use claw_context::{Block, BlockKind, ContextSink};
use claw_tool::ToolGroup;

use crate::agent::base_agent::{ContextAdapter, TurnLifecycle};
use crate::agent::effect::AgentEffectEmitter;
use crate::agent::recovery::AgentRecoverySnapshotBuilder;
use serde::{Deserialize, Serialize};

use self::tools::plan_tools;

mod tools;

const PLAN_MODE_FRAMING: &str = prompt!("plan_mode/instructions.md");

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentMode {
    #[default]
    Normal,
    Plan,
}

impl AgentMode {
    fn framing(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::Plan => PLAN_MODE_FRAMING,
        }
    }
}

pub(super) type SharedAgentMode = Arc<Mutex<AgentMode>>;

pub(super) fn lock_mode(mode: &SharedAgentMode) -> MutexGuard<'_, AgentMode> {
    mode.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Owns mode state, projects its context, and provides all mode-specific tools.
pub(crate) struct AgentModeContextAdapter {
    mode: SharedAgentMode,
    effects: AgentEffectEmitter,
}

impl AgentModeContextAdapter {
    pub(crate) fn new(initial: AgentMode, effects: AgentEffectEmitter) -> Self {
        Self {
            mode: Arc::new(Mutex::new(initial)),
            effects,
        }
    }
}

impl ContextAdapter for AgentModeContextAdapter {
    fn contribute(&mut self, output: &mut ContextSink<'_>) {
        let framing = lock_mode(&self.mode).framing();
        output.block(Block::new(BlockKind::ModeFraming, framing));
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(plan_tools(Arc::clone(&self.mode), self.effects.clone()))
    }

    fn on_turn_lifecycle(&mut self, lifecycle: TurnLifecycle) {
        match lifecycle {
            TurnLifecycle::Ended => *lock_mode(&self.mode) = AgentMode::Normal,
        }
    }

    fn contribute_recovery(&self, snapshot: &mut AgentRecoverySnapshotBuilder) {
        snapshot.set_mode(*lock_mode(&self.mode));
    }
}

#[cfg(test)]
mod tests {
    use claw_context::Context;

    use super::{AgentMode, AgentModeContextAdapter};
    use crate::agent::base_agent::{ContextAdapter, TurnLifecycle};
    use crate::agent::effect::agent_effect_channel;

    fn adapter(initial: AgentMode) -> AgentModeContextAdapter {
        let (effects, _inbox) = agent_effect_channel();
        AgentModeContextAdapter::new(initial, effects)
    }

    fn render(adapter: &mut AgentModeContextAdapter, context: &mut Context) -> String {
        let history = {
            let mut sink = context.sink();
            adapter.contribute(&mut sink);
            sink.into_history()
        };
        context.request(&history).system().to_owned()
    }

    #[test]
    fn plan_mode_projects_framing() {
        let mut adapter = adapter(AgentMode::Plan);
        let mut context = Context::new();
        assert!(render(&mut adapter, &mut context).contains("Do not implement"));
    }

    #[test]
    fn ended_turn_resets_adapter_owned_mode() {
        let mut adapter = adapter(AgentMode::Plan);
        adapter.on_turn_lifecycle(TurnLifecycle::Ended);

        let mut context = Context::new();
        assert_eq!(render(&mut adapter, &mut context), "");
    }
}
