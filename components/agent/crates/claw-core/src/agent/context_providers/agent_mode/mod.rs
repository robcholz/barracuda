//! Agent-mode provider: state, context projection, and lifecycle behavior.
//!
//! BaseAgent only drives the generic provider, effect, and task-lifecycle
//! protocols; mode is stored in the Agent's shared durable state. Turn
//! boundaries preserve that mode; the plan tools own explicit transitions.

use claw_context::{Block, BlockKind, ContextSink};
use claw_persistence::DurableState;
use claw_tool::ToolGroup;
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;

use crate::agent::base_agent::AgentEffectEmitter;
use crate::agent::base_agent::{ContextProvider, ContextProviderResult};
use crate::agent::BaseAgentState;

use self::tools::plan_tools;

mod tools;

const MODE_POLICY: &str = prompt!("plan_mode/instructions.md");

/// The context mode applied to the next model request.
#[derive(Clone, Copy, Debug, Deserialize, IntoStaticStr, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
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
        let mode: &'static str = self.state.get().mode().into();
        output
            .block(Block::new(BlockKind::ModePolicy, MODE_POLICY))
            .reminder(BlockKind::ActiveMode, Some(mode));
        Ok(())
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(plan_tools(self.state.clone(), self.effects.clone()))
    }
}

#[cfg(test)]
mod tests {
    use claw_context::Context;
    use claw_persistence::DurableState;
    use serde_json::Value;

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

    fn render(
        provider: &mut AgentModeContextProvider,
        context: &mut Context,
    ) -> (String, Vec<Value>) {
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(&mut sink).is_ok());
            sink.into_history()
        };
        let request = context.request(&history);
        (request.system().to_owned(), request.reminders().to_vec())
    }

    #[test]
    fn plan_mode_projects_static_policy_and_plan_reminder() {
        let mut provider = provider(AgentMode::Plan);
        let mut context = Context::new();
        let (system, reminders) = render(&mut provider, &mut context);

        assert!(system.contains("Do not implement"));
        assert_eq!(reminder_content(&reminders), Some("plan"));
    }

    #[test]
    fn ended_clarification_turn_preserves_plan_mode() {
        let mut provider = provider(AgentMode::Plan);
        provider.on_turn_lifecycle(TurnLifecycle::Ended);

        let mut context = Context::new();
        let (_, reminders) = render(&mut provider, &mut context);
        assert_eq!(reminder_content(&reminders), Some("plan"));
    }

    #[test]
    fn mode_switch_changes_only_active_mode_reminder() {
        let mut provider = provider(AgentMode::Normal);
        let mut context = Context::new();
        let (normal_system, normal_reminders) = render(&mut provider, &mut context);
        let version = context.version();

        provider.state.get_mut().set_mode(AgentMode::Plan);
        let (plan_system, plan_reminders) = render(&mut provider, &mut context);

        assert_eq!(normal_system, plan_system);
        assert_eq!(context.version(), version);
        assert_eq!(reminder_content(&normal_reminders), Some("normal"));
        assert_eq!(reminder_content(&plan_reminders), Some("plan"));
    }

    fn reminder_content(reminders: &[Value]) -> Option<&str> {
        reminders
            .first()
            .and_then(|reminder| reminder.get("content"))
            .and_then(Value::as_str)
            .and_then(|content| {
                content
                    .strip_prefix("<system-reminder>\n")
                    .and_then(|content| content.strip_suffix("\n</system-reminder>"))
            })
    }
}
