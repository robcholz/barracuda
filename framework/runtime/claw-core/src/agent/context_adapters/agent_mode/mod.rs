//! Agent-mode adapter: state, context projection, and lifecycle behavior.
//!
//! Mode is adapter-local state. BaseAgent only drives the generic adapter,
//! effect, and task-lifecycle protocols.

use std::sync::{Arc, Mutex, MutexGuard};

use claw_context::{Block, BlockKind, ContextSink};
use claw_tool::ToolGroup;
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::AgentEffectEmitter;
use crate::agent::base_agent::{AgentStateBuilder, ContextAdapter, TurnLifecycle};

use self::tools::plan_tools;

mod tools;

const PLAN_MODE_FRAMING: &str = prompt!("plan_mode/instructions.md");

/// Durable state owned by the Agent-mode context adapter.
///
/// This DTO deliberately has no `Default`; a missing persisted Agent state is
/// passed to the adapter as `None`, and the adapter owns its initialization
/// policy.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::agent) enum AgentModeState {
    Normal,
    Plan,
}

impl AgentModeState {
    fn framing(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::Plan => PLAN_MODE_FRAMING,
        }
    }
}

pub(super) type SharedAgentMode = Arc<Mutex<AgentModeState>>;

pub(super) fn lock_mode(mode: &SharedAgentMode) -> MutexGuard<'_, AgentModeState> {
    mode.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Owns mode state, projects its context, and provides all mode-specific tools.
pub(crate) struct AgentModeContextAdapter {
    mode: SharedAgentMode,
    effects: AgentEffectEmitter,
}

impl AgentModeContextAdapter {
    pub(crate) fn new(state: Option<AgentModeState>, effects: AgentEffectEmitter) -> Self {
        Self {
            mode: Arc::new(Mutex::new(state.unwrap_or(AgentModeState::Normal))),
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
            TurnLifecycle::Ended => *lock_mode(&self.mode) = AgentModeState::Normal,
        }
    }

    fn contribute_state(&self, state: &mut AgentStateBuilder) {
        state.set_agent_mode(*lock_mode(&self.mode));
    }
}

#[cfg(test)]
mod tests {
    use claw_context::Context;

    use super::{AgentModeContextAdapter, AgentModeState};
    use crate::agent::base_agent::agent_effect_channel;
    use crate::agent::base_agent::{ContextAdapter, TurnLifecycle};

    fn adapter(state: Option<AgentModeState>) -> AgentModeContextAdapter {
        let (effects, _inbox) = agent_effect_channel();
        AgentModeContextAdapter::new(state, effects)
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
        let mut adapter = adapter(Some(AgentModeState::Plan));
        let mut context = Context::new();
        assert!(render(&mut adapter, &mut context).contains("Do not implement"));
    }

    #[test]
    fn ended_turn_resets_adapter_owned_mode() {
        let mut adapter = adapter(Some(AgentModeState::Plan));
        adapter.on_turn_lifecycle(TurnLifecycle::Ended);

        let mut context = Context::new();
        assert_eq!(render(&mut adapter, &mut context), "");
    }

    #[test]
    fn missing_state_uses_the_adapter_owned_initial_mode() {
        let mut adapter = adapter(None);
        let mut context = Context::new();

        assert_eq!(render(&mut adapter, &mut context), "");
    }
}
