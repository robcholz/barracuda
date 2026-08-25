//! Agent-mode provider: state, context projection, and lifecycle behavior.
//!
//! AgentEngine only drives the generic provider, effect, and task-lifecycle
//! protocols. The provider and its tools share the `plan` provider object;
//! turn boundaries preserve that mode and the plan tools own transitions.

use crate::engine::AgentStorage;
use barracuda_agent_context::{Block, BlockKind, ContextSink};
use barracuda_agent_tool::{Tool, ToolGroup};
use serde_json::json;
use strum::IntoStaticStr;

use self::tools::{EnterPlanModeTool, ExitPlanModeTool, RequestClarificationTool};
use crate::engine::AgentEffectEmitter;
use crate::engine::{ContextProvider, ContextProviderResult};

mod tools;

const MODE_POLICY: &str = prompt!("plan_mode/instructions.md");

/// The context mode applied to the next model request.
#[derive(Clone, Copy, Debug, IntoStaticStr, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum AgentMode {
    Normal,
    Plan,
}

/// Projects the shared Agent mode and provides all mode-specific tools.
pub(crate) struct AgentModeContextProvider {
    effects: AgentEffectEmitter,
}

impl AgentModeContextProvider {
    pub(crate) fn new(effects: AgentEffectEmitter) -> Self {
        Self { effects }
    }
}

impl ContextProvider for AgentModeContextProvider {
    fn id(&self) -> &'static str {
        "plan"
    }

    fn contribute(
        &mut self,
        storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        let mode: &'static str = load_mode(storage).into();
        output
            .block(Block::new(BlockKind::ModePolicy, MODE_POLICY))
            .reminder(BlockKind::ActiveMode, Some(mode));
        Ok(())
    }

    fn tools(&self, storage: &AgentStorage) -> Option<ToolGroup> {
        Some(ToolGroup::new(
            self.id(),
            true,
            [
                Tool::new(EnterPlanModeTool {
                    storage: storage.clone(),
                }),
                Tool::new(RequestClarificationTool {
                    effects: self.effects.clone(),
                }),
                Tool::new(ExitPlanModeTool {
                    effects: self.effects.clone(),
                    storage: storage.clone(),
                }),
            ],
        ))
    }
}

fn load_mode(storage: &AgentStorage) -> AgentMode {
    match storage
        .load()
        .as_ref()
        .and_then(|object| object.get("mode"))
        .and_then(|mode| mode.as_str())
    {
        Some("plan") => AgentMode::Plan,
        _ => AgentMode::Normal,
    }
}

fn store_mode(storage: &AgentStorage, mode: AgentMode) {
    let mode: &'static str = mode.into();
    storage.store(json!({ "mode": mode }));
}

#[cfg(test)]
mod tests {
    use barracuda_agent_context::Context;
    use barracuda_agent_persistence::DurableState;
    use serde_json::Value;

    use super::{store_mode, AgentMode, AgentModeContextProvider};
    use crate::engine::{agent_effect_channel, AgentStorage};
    use crate::engine::{ContextProvider, TurnLifecycle};
    use crate::{AgentEngineState, AgentKind};

    fn provider(mode: AgentMode) -> (AgentModeContextProvider, AgentStorage) {
        let state = DurableState::new(AgentEngineState::new(&AgentKind::from_static("worker")));
        let (effects, _inbox) = agent_effect_channel();
        let provider = AgentModeContextProvider::new(effects);
        let storage = AgentStorage::new(&state, provider.id());
        store_mode(&storage, mode);
        (provider, storage)
    }

    fn render(
        provider: &mut AgentModeContextProvider,
        storage: &AgentStorage,
        context: &mut Context,
    ) -> (String, Vec<Value>) {
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(storage, &mut sink).is_ok());
            sink.into_history()
        };
        let request = context.request(&history);
        (request.system().to_owned(), request.reminders().to_vec())
    }

    #[test]
    fn plan_mode_projects_static_policy_and_plan_reminder() {
        let (mut provider, storage) = provider(AgentMode::Plan);
        let mut context = Context::new();
        let (system, reminders) = render(&mut provider, &storage, &mut context);

        assert!(system.contains("Do not implement"));
        assert_eq!(reminder_content(&reminders), Some("plan"));
    }

    #[test]
    fn ended_clarification_turn_preserves_plan_mode() {
        let (mut provider, storage) = provider(AgentMode::Plan);
        provider.on_turn_lifecycle(&storage, TurnLifecycle::Ended);

        let mut context = Context::new();
        let (_, reminders) = render(&mut provider, &storage, &mut context);
        assert_eq!(reminder_content(&reminders), Some("plan"));
    }

    #[test]
    fn mode_switch_changes_only_active_mode_reminder() {
        let (mut provider, storage) = provider(AgentMode::Normal);
        let mut context = Context::new();
        let (normal_system, normal_reminders) = render(&mut provider, &storage, &mut context);
        let version = context.version();

        store_mode(&storage, AgentMode::Plan);
        let (plan_system, plan_reminders) = render(&mut provider, &storage, &mut context);

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
