//! One-shot resume context plus the tool-discovery surface.

use alloc::{borrow::Cow, string::String, vec::Vec};

use barracuda_agent_context::{Band, BlockKind, ContextSink, Scope};
use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::{ToolDiscoveryHandle, ToolGroup};

use crate::agent::base_agent::{ContextProvider, ContextProviderResult};
use crate::agent::BaseAgentState;

use self::tools::discovery_tools;

mod tools;

/// Contributes a one-shot reminder derived from restored Agent state and
/// exposes tool discovery.
pub(in crate::agent) struct ResumeContextProvider {
    state: DurableState<BaseAgentState>,
    reminder: Option<String>,
    discovery: ToolDiscoveryHandle,
}

impl ResumeContextProvider {
    pub(in crate::agent) fn new(
        state: DurableState<BaseAgentState>,
        discovery: ToolDiscoveryHandle,
    ) -> Self {
        let reminder = render_resume_reminder(&state.get());
        Self {
            state,
            reminder,
            discovery,
        }
    }
}

impl ContextProvider for ResumeContextProvider {
    fn contribute(&mut self, output: &mut ContextSink<'_>) -> ContextProviderResult {
        let reminder = self.reminder.take();
        output.reminder(resume_reminder_kind(), reminder.as_deref());
        Ok(())
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(discovery_tools(self.discovery.clone(), self.state.clone()))
    }
}

fn render_resume_reminder(state: &BaseAgentState) -> Option<String> {
    let mut details = Vec::new();
    if !state.loaded_tool_groups().is_empty() {
        details.push(format!(
            "previously loaded tool groups: {}",
            state
                .loaded_tool_groups()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let inflight_toolcalls = state.inflight_toolcalls();
    let has_inflight_toolcalls = !inflight_toolcalls.is_empty();
    if has_inflight_toolcalls {
        let calls = inflight_toolcalls
            .iter()
            .map(|call| format!("{}({})", call.name, call.arguments_json))
            .collect::<Vec<_>>()
            .join(", ");
        details.push(format!(
            "tool calls with unknown completion status: {calls}"
        ));
    }

    (!details.is_empty()).then(|| {
        let caution = if has_inflight_toolcalls {
            " Inflight tool calls were not replayed; inspect current external state before relying on their completion."
        } else {
            ""
        };
        format!(
            "Session resumed after a restart; {}.{caution}",
            details.join("; ")
        )
    })
}

fn resume_reminder_kind() -> BlockKind {
    BlockKind::Custom {
        band: Band::Volatile,
        scope: Scope::Agent,
        order: 1,
        label: Cow::Borrowed("resume"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::ResumeContextProvider;
    use crate::agent::base_agent::ContextProvider;
    use crate::agent::{AgentKind, BaseAgentState};
    use barracuda_agent_context::Context;
    use barracuda_agent_persistence::DurableState;
    use barracuda_agent_tool::{
        Tool, ToolFuture, ToolGroup, ToolHandler, ToolOutput, ToolSet, ToolSpec,
    };
    use barracuda_model_api::ToolCall;

    #[test]
    fn resume_context_is_contributed_once_while_discovery_tools_remain_available() {
        let mut tool_set = ToolSet::empty();
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().record_inflight_toolcalls(vec![ToolCall {
            id: "call-1".to_owned(),
            name: "profile_read".to_owned(),
            arguments_json: r#"{"document":"user"}"#.to_owned(),
        }]);
        let mut provider = ResumeContextProvider::new(state, tool_set.discovery());
        tool_set
            .add_group(provider.tools().expect("discovery group exists"))
            .expect("discovery group attaches");
        let tools = tool_set.begin().expect("tool set begins");
        let schemas = tools.static_schemas();
        assert!(schemas.contains("tool_search"));
        assert!(schemas.contains("tool_load"));

        let mut context = Context::new();
        let first = {
            let mut sink = context.sink();
            assert!(provider.contribute(&mut sink).is_ok());
            sink.into_history()
        };
        let request = context.request(&first);
        let reminder = request.reminders().first().expect("resume reminder exists");
        assert!(reminder
            .to_string()
            .contains("Session resumed after a restart"));

        let second = {
            let mut sink = context.sink();
            assert!(provider.contribute(&mut sink).is_ok());
            sink.into_history()
        };
        assert!(context.request(&second).reminders().is_empty());
    }

    #[test]
    fn restored_tool_groups_are_reminded_without_loading_tools() {
        let mut tool_set = ToolSet::empty();
        tool_set
            .add_group(ToolGroup::new("hidden", false, [Tool::new(HiddenTool)]))
            .expect("hidden group registers");
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state
            .get_mut()
            .record_loaded_tool_group("hidden".to_owned());
        let mut provider = ResumeContextProvider::new(state, tool_set.discovery());
        tool_set
            .add_group(provider.tools().expect("discovery group exists"))
            .expect("discovery group attaches");
        let tools = tool_set.begin().expect("tool set begins");
        assert!(!tools.static_schemas().contains("hidden_test"));

        let mut context = Context::new();
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(&mut sink).is_ok());
            sink.into_history()
        };
        let request = context.request(&history);
        assert!(request.reminders()[0]
            .to_string()
            .contains("previously loaded tool groups: hidden"));
    }

    struct HiddenTool;

    impl ToolSpec for HiddenTool {
        fn name(&self) -> &str {
            "hidden_test"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"hidden_test"}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("resources/tools/plan_enter/schema.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for HiddenTool {
        type Args = barracuda_agent_tool::EmptyArgs;

        fn invoke<'a>(
            &'a self,
            _context: barracuda_agent_tool::ToolContext,
            _args: Self::Args,
        ) -> ToolFuture<'a> {
            alloc::boxed::Box::pin(async {
                Ok(ToolOutput {
                    content: "ok".to_owned(),
                    ok: true,
                })
            })
        }
    }
}
