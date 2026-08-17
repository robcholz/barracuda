//! The `tool_discovery` group: search hidden groups and load one for the next turn.

use alloc::{borrow::ToOwned, string::String, string::ToString};

use crate::agent::base_agent::AgentStorage;
use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, ToolDiscoveryHandle, ToolFuture, ToolHandler, ToolInvocation,
    ToolOutput, ToolSpec,
};
use serde::Deserialize;
use serde_json::json;

use super::record_loaded_tool_group;

#[derive(Deserialize)]
pub(super) struct LoadArgs {
    group_id: String,
}

/// Reads the owning [`ToolSet`](barracuda_agent_tool::ToolSet)'s loadable catalog and
/// returns it — group ids, tool names, and short descriptions, never schemas.
pub(super) struct ToolSearchTool {
    pub(super) discovery: ToolDiscoveryHandle,
}

/// Queues a group to be enabled when ToolSet begins the next iteration.
pub(super) struct ToolLoadTool {
    pub(super) discovery: ToolDiscoveryHandle,
    pub(super) storage: AgentStorage,
}

impl ToolSpec for ToolLoadTool {
    tool_metadata!("tool_load");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ToolLoadTool {
    type Args = LoadArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let group_id = args.group_id.trim().to_owned();
            let loaded = self.discovery.request_load(group_id.clone());
            if loaded {
                record_loaded_tool_group(&self.storage, group_id.clone());
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

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
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
    use barracuda_agent_persistence::DurableState;
    use barracuda_agent_tool::{
        Tool, ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolOutput, ToolRunner, ToolSet,
        ToolSpec,
    };
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use crate::agent::base_agent::{AgentStorage, ContextProvider};
    use crate::agent::context_providers::tool_discovery::{
        loaded_tool_groups, ToolDiscoveryContextProvider,
    };
    use crate::agent::{AgentKind, BaseAgentState};

    #[test]
    fn successful_load_is_recorded_in_the_discovery_provider_object() {
        let mut tool_set = ToolSet::empty();
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        tool_set
            .add_group(ToolGroup::new("hidden", false, [Tool::new(HiddenTool)]))
            .expect("hidden group registers");
        let discovery = tool_set.discovery();
        let provider = ToolDiscoveryContextProvider::new(discovery.clone());
        let storage = AgentStorage::new(&state, provider.id());
        tool_set
            .add_group(
                provider
                    .tools(&storage)
                    .expect("provider exposes discovery tools"),
            )
            .expect("discovery group registers");
        let tools = tool_set.begin().expect("tool set begins");
        let call =
            ToolInvocation::try_new(Some("call-test"), "tool_load", r#"{"group_id":"hidden"}"#)
                .expect("valid invocation");

        let (joined, detached) = ToolRunner::new(&tools).run(vec![call]);
        assert!(detached.is_none());
        let output = block_on(joined.collect::<Vec<_>>())
            .pop()
            .expect("load result")
            .1;

        assert!(output.ok);
        assert_eq!(loaded_tool_groups(&storage), vec!["hidden".to_owned()]);
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

        fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
            alloc::boxed::Box::pin(async {
                Ok(ToolOutput {
                    content: "ok".to_owned(),
                    ok: true,
                })
            })
        }
    }
}
