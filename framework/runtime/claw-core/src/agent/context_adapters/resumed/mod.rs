//! One-shot resume context plus the tool-discovery surface.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use claw_context::{Band, BlockKind, ContextSink, Scope};
use claw_tool::{ToolDiscoveryHandle, ToolGroup};
use serde::{Deserialize, Serialize};

use crate::agent::base_agent::{AgentStateBuilder, ContextAdapter, InflightToolCall};

use self::tools::discovery_tools;

mod tools;

/// Durable state owned by [`ResumedContextAdapter`].
///
/// The DTO has no `Default`: Factory passes `None` for a fresh Agent, and the
/// adapter owns its explicit empty-state policy.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::agent) struct ResumedState {
    loaded_tool_groups: BTreeSet<String>,
}

impl ResumedState {
    pub(in crate::agent) fn new(loaded_tool_groups: impl IntoIterator<Item = String>) -> Self {
        Self {
            loaded_tool_groups: loaded_tool_groups.into_iter().collect(),
        }
    }

    pub(super) fn record_loaded_tool_group(&mut self, group_id: String) {
        self.loaded_tool_groups.insert(group_id);
    }
}

pub(super) type SharedResumedState = Arc<Mutex<ResumedState>>;

pub(super) fn lock_state(state: &SharedResumedState) -> MutexGuard<'_, ResumedState> {
    state.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Process-local information shown once after reconstructing an Agent.
///
/// This is not durable Agent state. Factory derives it while distributing a
/// restored AgentState to its authoritative components.
pub(in crate::agent) struct AgentResumeNotice {
    inflight_toolcalls: Vec<InflightToolCall>,
}

impl AgentResumeNotice {
    pub(in crate::agent) fn new(inflight_toolcalls: Vec<InflightToolCall>) -> Self {
        Self { inflight_toolcalls }
    }
}

/// Contributes the one-shot resume reminder and exposes tool discovery.
///
/// The discovery group is part of this adapter because the same component also
/// contributes optional resume context.
pub(in crate::agent) struct ResumedContextAdapter {
    state: SharedResumedState,
    reminder: Option<String>,
    discovery: ToolDiscoveryHandle,
}

impl ResumedContextAdapter {
    pub(in crate::agent) fn new(
        state: Option<ResumedState>,
        notice: Option<AgentResumeNotice>,
        discovery: ToolDiscoveryHandle,
    ) -> Self {
        let state = state.unwrap_or_else(|| ResumedState::new(Vec::new()));
        Self {
            reminder: render_resume_reminder(&state, notice),
            state: Arc::new(Mutex::new(state)),
            discovery,
        }
    }
}

impl ContextAdapter for ResumedContextAdapter {
    fn contribute(&mut self, output: &mut ContextSink<'_>) {
        let reminder = self.reminder.take();
        output.reminder(resume_reminder_kind(), reminder.as_deref());
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(discovery_tools(
            self.discovery.clone(),
            Arc::clone(&self.state),
        ))
    }

    fn contribute_state(&self, state: &mut AgentStateBuilder) {
        state.set_resumed(lock_state(&self.state).clone());
    }
}

fn render_resume_reminder(
    state: &ResumedState,
    notice: Option<AgentResumeNotice>,
) -> Option<String> {
    let mut details = Vec::new();
    if !state.loaded_tool_groups.is_empty() {
        details.push(format!(
            "previously loaded tool groups: {}",
            state
                .loaded_tool_groups
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let inflight_toolcalls = notice
        .map(|notice| notice.inflight_toolcalls)
        .unwrap_or_default();
    let has_inflight_toolcalls = !inflight_toolcalls.is_empty();
    if has_inflight_toolcalls {
        let calls = inflight_toolcalls
            .iter()
            .map(|call| format!("{}({})", call.tool(), call.arguments()))
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
mod tests {
    use std::sync::Arc;

    use claw_context::Context;
    use claw_tool::{
        SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolRegistry, ToolResult,
        ToolSpec,
    };
    use serde_json::json;

    use super::{AgentResumeNotice, ResumedContextAdapter, ResumedState};
    use crate::agent::base_agent::{ContextAdapter, InflightToolCall};

    #[test]
    fn resume_context_is_contributed_once_while_discovery_tools_remain_available() {
        let registry = Arc::new(ToolRegistry::new());
        let mut tool_set = registry.tool_set();
        let mut adapter = ResumedContextAdapter::new(
            None,
            Some(AgentResumeNotice::new(vec![InflightToolCall::new(
                "profile_read",
                json!({"document":"user"}),
            )])),
            tool_set.discovery(),
        );
        tool_set
            .add_group(adapter.tools().expect("discovery group exists"))
            .expect("discovery group attaches");
        let tools = tool_set.begin().expect("tool set begins");
        let schemas = tools.schemas_json();
        assert!(schemas.contains("tool_search"));
        assert!(schemas.contains("tool_load"));

        let mut context = Context::new();
        let first = {
            let mut sink = context.sink();
            adapter.contribute(&mut sink);
            sink.into_history()
        };
        let request = context.request(&first);
        let reminder = request.reminders().first().expect("resume reminder exists");
        assert!(reminder
            .to_string()
            .contains("Session resumed after a restart"));

        let second = {
            let mut sink = context.sink();
            adapter.contribute(&mut sink);
            sink.into_history()
        };
        assert!(context.request(&second).reminders().is_empty());
    }

    #[test]
    fn restored_state_is_reminded_without_loading_tools() {
        let registry = Arc::new(ToolRegistry::new());
        registry
            .register_group(ToolGroup::new(
                "hidden",
                false,
                [Tool::from_sync(HiddenTool)],
            ))
            .expect("hidden group registers");
        registry.start_all().expect("registry starts");
        let mut tool_set = registry.tool_set();
        let mut adapter = ResumedContextAdapter::new(
            Some(ResumedState::new(vec!["hidden".to_owned()])),
            None,
            tool_set.discovery(),
        );
        tool_set
            .add_group(adapter.tools().expect("discovery group exists"))
            .expect("discovery group attaches");
        let tools = tool_set.begin().expect("tool set begins");
        assert!(!tools.schemas_json().contains("hidden_test"));

        let mut context = Context::new();
        let history = {
            let mut sink = context.sink();
            adapter.contribute(&mut sink);
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
    }

    impl SyncToolHandler for HiddenTool {
        fn invoke(&self, _call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
            Ok(ToolOutput {
                output: "ok".to_owned(),
                ok: true,
            })
        }
    }
}
