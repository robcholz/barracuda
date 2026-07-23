//! Agent-mode adapter: state, context projection, and lifecycle behavior.
//!
//! BaseAgent only drives the generic adapter, effect, and task-lifecycle
//! protocols; mode is stored in the Agent's shared durable state.

use claw_context::{Block, BlockKind, ContextSink};
use claw_persistence::DurableState;
use claw_tool::ToolGroup;
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::AgentEffectEmitter;
use crate::agent::base_agent::{ContextAdapter, TurnLifecycle};
use crate::agent::BaseAgentState;

use self::tools::plan_tools;

mod tools;

const PLAN_MODE_FRAMING: &str = prompt!("plan_mode/instructions.md");

/// The context mode applied to the next model request.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::agent) enum AgentMode {
    Normal,
    Plan,
}

/// Projects the shared Agent mode and provides all mode-specific tools.
pub(crate) struct AgentModeContextAdapter {
    state: DurableState<BaseAgentState>,
    effects: AgentEffectEmitter,
}

impl AgentModeContextAdapter {
    pub(crate) fn new(state: DurableState<BaseAgentState>, effects: AgentEffectEmitter) -> Self {
        Self { state, effects }
    }
}

impl ContextAdapter for AgentModeContextAdapter {
    fn contribute(&mut self, output: &mut ContextSink<'_>) {
        let framing = match self.state.get().mode() {
            AgentMode::Normal => "",
            AgentMode::Plan => PLAN_MODE_FRAMING,
        };
        output.block(Block::new(BlockKind::ModeFraming, framing));
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(plan_tools(self.state.clone(), self.effects.clone()))
    }

    fn on_turn_lifecycle(&mut self, lifecycle: TurnLifecycle) {
        match lifecycle {
            TurnLifecycle::Ended => self.state.get_mut().set_mode(AgentMode::Normal),
        }
    }
}

#[cfg(test)]
mod tests {
    use claw_context::Context;
    use claw_persistence::DurableState;

    use super::{AgentMode, AgentModeContextAdapter};
    use crate::agent::base_agent::agent_effect_channel;
    use crate::agent::base_agent::{ContextAdapter, TurnLifecycle};
    use crate::agent::{AgentKind, BaseAgentState};

    fn adapter(mode: AgentMode) -> AgentModeContextAdapter {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().set_mode(mode);
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
        let mut adapter = adapter(AgentMode::Plan);
        let mut context = Context::new();
        assert!(render(&mut adapter, &mut context).contains("Do not implement"));
    }

    #[test]
    fn ended_turn_resets_shared_agent_mode() {
        let mut adapter = adapter(AgentMode::Plan);
        adapter.on_turn_lifecycle(TurnLifecycle::Ended);

        let mut context = Context::new();
        assert_eq!(render(&mut adapter, &mut context), "");
    }

    #[test]
    fn normal_mode_has_no_framing() {
        let mut adapter = adapter(AgentMode::Normal);
        let mut context = Context::new();

        assert_eq!(render(&mut adapter, &mut context), "");
    }
}
