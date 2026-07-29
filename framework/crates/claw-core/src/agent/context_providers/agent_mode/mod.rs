//! Agent-mode provider: state, context projection, and lifecycle behavior.
//!
//! BaseAgent only drives the generic provider, effect, and task-lifecycle
//! protocols; mode is stored in the Agent's shared durable state.

use claw_context::{Block, BlockKind, ContextSink};
use claw_persistence::DurableState;
use claw_tool::ToolGroup;
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::AgentEffectEmitter;
use crate::agent::base_agent::{ContextProvider, ContextProviderResult, TurnLifecycle};
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
pub(crate) struct AgentModeContextProvider {
    state: DurableState<BaseAgentState>,
    effects: AgentEffectEmitter,
}

impl AgentModeContextProvider {
    pub(crate) fn new(state: DurableState<BaseAgentState>, effects: AgentEffectEmitter) -> Self {
        Self { state, effects }
    }
}

impl ContextProvider for AgentModeContextProvider {
    fn contribute(&mut self, output: &mut ContextSink<'_>) -> ContextProviderResult {
        let framing = match self.state.get().mode() {
            AgentMode::Normal => "",
            AgentMode::Plan => PLAN_MODE_FRAMING,
        };
        output.block(Block::new(BlockKind::ModeFraming, framing));
        Ok(())
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

    use super::{AgentMode, AgentModeContextProvider};
    use crate::agent::base_agent::agent_effect_channel;
    use crate::agent::base_agent::{ContextProvider, TurnLifecycle};
    use crate::agent::{AgentKind, BaseAgentState};

    fn provider(mode: AgentMode) -> AgentModeContextProvider {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().set_mode(mode);
        let (effects, _inbox) = agent_effect_channel();
        AgentModeContextProvider::new(state, effects)
    }

    fn render(provider: &mut AgentModeContextProvider, context: &mut Context) -> String {
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(&mut sink).is_ok());
            sink.into_history()
        };
        context.request(&history).system().to_owned()
    }

    #[test]
    fn plan_mode_projects_framing() {
        let mut provider = provider(AgentMode::Plan);
        let mut context = Context::new();
        assert!(render(&mut provider, &mut context).contains("Do not implement"));
    }

    #[test]
    fn ended_turn_resets_shared_agent_mode() {
        let mut provider = provider(AgentMode::Plan);
        provider.on_turn_lifecycle(TurnLifecycle::Ended);

        let mut context = Context::new();
        assert_eq!(render(&mut provider, &mut context), "");
    }

    #[test]
    fn normal_mode_has_no_framing() {
        let mut provider = provider(AgentMode::Normal);
        let mut context = Context::new();

        assert_eq!(render(&mut provider, &mut context), "");
    }
}
