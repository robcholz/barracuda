//! The `tool_discovery` group: search hidden groups and load one for the next turn.

use claw_permission::{Action, RiskClass};
use claw_persistence::DurableState;
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolDiscoveryHandle, ToolError, ToolGroup,
    ToolInvocation, ToolInvokeError, ToolOutput, ToolSpec,
};
use serde_json::json;

use crate::agent::tools::helper::optional_string_argument;
use crate::agent::BaseAgentState;

/// Build the always-visible discovery group over a [`ToolSet`](claw_tool::ToolSet)
/// bridge. All other registered groups remain hidden until `tool_load` reveals
/// one for the next turn.
pub(super) fn discovery_tools(
    discovery: ToolDiscoveryHandle,
    state: DurableState<BaseAgentState>,
) -> ToolGroup {
    ToolGroup::new(
        "tool_discovery",
        true,
        [
            Tool::from_sync(ToolSearchTool {
                discovery: discovery.clone(),
            }),
            Tool::from_sync(ToolLoadTool { discovery, state }),
        ],
    )
}

/// Reads the owning [`ToolSet`](claw_tool::ToolSet)'s loadable catalog and
/// returns it — group ids, tool names, and short descriptions, never schemas.
struct ToolSearchTool {
    discovery: ToolDiscoveryHandle,
}

/// Queues a group to be enabled when ToolSet begins the next iteration.
struct ToolLoadTool {
    discovery: ToolDiscoveryHandle,
    state: DurableState<BaseAgentState>,
}

impl ToolSpec for ToolLoadTool {
    tool_metadata!("tool_load");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for ToolLoadTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let args = call.arguments_value()?;
        let group_id = optional_string_argument(&args, "group_id")?
            .map(|group_id| group_id.trim().to_owned())
            .filter(|group_id| !group_id.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidArguments("tool_load 'group_id' is required".into())
            })?;

        let loaded = self.discovery.request_load(group_id.clone());
        if loaded {
            self.state
                .get_mut()
                .record_loaded_tool_group(group_id.clone());
        }
        Ok(ToolOutput {
            output: json!({
                "group_id": group_id,
                "loaded": loaded,
                "available_next_turn": loaded,
            })
            .to_string(),
            ok: loaded,
        })
    }
}

impl ToolSpec for ToolSearchTool {
    tool_metadata!("tool_search");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for ToolSearchTool {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        Ok(ToolOutput {
            output: json!({ "tool_groups": self.discovery.catalog() }).to_string(),
            ok: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use claw_persistence::DurableState;
    use claw_tool::{
        SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolRegistry, ToolResult,
        ToolSpec,
    };

    use super::ToolLoadTool;
    use crate::agent::{AgentKind, BaseAgentState};

    #[test]
    fn successful_load_is_recorded_in_agent_state() {
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
        let discovery = tool_set.discovery();
        {
            let _initial_tools = tool_set.begin().expect("tool set begins");
        }
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        let tool = ToolLoadTool {
            discovery,
            state: state.clone(),
        };
        let call =
            ToolInvocation::try_new(Some("call-test"), "tool_load", r#"{"group_id":"hidden"}"#)
                .expect("valid invocation");

        let output = tool.invoke(&call).expect("load succeeds");

        assert!(output.ok);
        assert!(state.get().loaded_tool_groups().contains("hidden"));
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
