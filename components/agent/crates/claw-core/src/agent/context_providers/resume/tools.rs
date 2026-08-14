//! The `tool_discovery` group: search hidden groups and load one for the next turn.

use alloc::string::String;

use claw_permission::{Action, RiskClass};
use claw_persistence::DurableState;
use claw_tool::{
    tool_metadata, EmptyArgs, Tool, ToolDiscoveryHandle, ToolFuture, ToolGroup, ToolHandler,
    ToolInvocation, ToolOutput, ToolSpec,
};
use serde::Deserialize;
use serde_json::json;

use crate::agent::BaseAgentState;

#[derive(Deserialize)]
struct LoadArgs {
    group_id: String,
}

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
            Tool::new(ToolSearchTool {
                discovery: discovery.clone(),
            }),
            Tool::new(ToolLoadTool { discovery, state }),
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

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ToolLoadTool {
    type Args = LoadArgs;

    fn invoke<'a>(&'a self, _context: claw_tool::ToolContext, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let group_id = args.group_id.trim().to_owned();
            let loaded = self.discovery.request_load(group_id.clone());
            if loaded {
                self.state
                    .get_mut()
                    .record_loaded_tool_group(group_id.clone());
            }
            Ok(ToolOutput {
                content: json!({
                    "group_id": group_id,
                    "loaded": loaded,
                    "available_next_turn": loaded,
                })
                .to_string(),
                ok: loaded,
            })
        })
    }
}

impl ToolSpec for ToolSearchTool {
    tool_metadata!("tool_search");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ToolSearchTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _context: claw_tool::ToolContext, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            Ok(ToolOutput {
                content: json!({ "tool_groups": self.discovery.catalog() }).to_string(),
                ok: true,
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use claw_persistence::DurableState;
    use claw_tool::{
        Tool, ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolOutput, ToolSet, ToolSpec,
    };
    use futures_lite::future::block_on;

    use super::ToolLoadTool;
    use crate::agent::{AgentKind, BaseAgentState};

    #[test]
    fn successful_load_is_recorded_in_agent_state() {
        let mut tool_set = ToolSet::empty();
        tool_set
            .add_group(ToolGroup::new("hidden", false, [Tool::new(HiddenTool)]))
            .expect("hidden group registers");
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

        let args = call.arguments().expect("valid load args");
        let output = block_on(tool.invoke(claw_tool::ToolContext::stateless(), args))
            .expect("load succeeds");

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

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("resources/tools/plan_enter/schema.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for HiddenTool {
        type Args = claw_tool::EmptyArgs;

        fn invoke<'a>(
            &'a self,
            _context: claw_tool::ToolContext,
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
use alloc::{borrow::ToOwned, string::ToString};
