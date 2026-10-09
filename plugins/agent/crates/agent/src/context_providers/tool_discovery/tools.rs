//! The `tool_discovery` group: search hidden groups and load one for the next turn.

use alloc::{borrow::ToOwned, string::String, string::ToString};

use crate::engine::AgentStorage;
use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, ToolDiscoveryHandle, ToolFuture, ToolHandler, ToolInvocation,
    ToolOutput, ToolSpec,
};
use serde::Deserialize;
use serde_json::json;

use super::{loaded_tool_groups, record_loaded_tool_group};

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
            if self.discovery.request_load(group_id.clone()) {
                record_loaded_tool_group(&self.storage, group_id.clone());
                return Ok(ToolOutput {
                    content: json!({
                        "group_id": group_id,
                        "loaded": true,
                        "available_next_turn": true,
                    })
                    .to_string(),
                    ok: true,
                });
            }
            // A loaded group leaves the loadable catalog. Reporting it as not
            // loaded makes a model retry the load instead of calling its tools.
            if loaded_tool_groups(&self.storage).contains(&group_id) {
                return Ok(ToolOutput {
                    content: json!({
                        "group_id": group_id,
                        "loaded": true,
                        "already_loaded": true,
                    })
                    .to_string(),
                    ok: true,
                });
            }
            Ok(ToolOutput {
                content: json!({
                    "group_id": group_id,
                    "loaded": false,
                    "error": "unknown tool group; tool_search lists the loadable groups",
                })
                .to_string(),
                ok: false,
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

    use crate::context_providers::tool_discovery::{
        loaded_tool_groups, ToolDiscoveryContextProvider,
    };
    use crate::engine::{AgentStorage, ContextProvider};
    use crate::{AgentEngineState, AgentKind};

    #[test]
    fn successful_load_is_recorded_in_the_discovery_provider_object() {
        let mut tool_set = ToolSet::empty();
        let state = DurableState::new(AgentEngineState::new(&AgentKind::from_static("worker")));
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

        let joined = ToolRunner::new(&tools).run(vec![call]);
        let output = block_on(joined.collect::<Vec<_>>())
            .pop()
            .expect("load result")
            .1;

        assert!(output.ok);
        assert_eq!(loaded_tool_groups(&storage), vec!["hidden".to_owned()]);

        // Once the next iteration applies the load, the group is no longer
        // loadable; loading it again reports it as already loaded.
        let tools = tool_set.begin().expect("tool set begins again");
        let load_again = |group: &str| {
            let call = ToolInvocation::try_new(
                Some("call-again"),
                "tool_load",
                &alloc::format!(r#"{{"group_id":"{group}"}}"#),
            )
            .expect("valid invocation");
            let joined = ToolRunner::new(&tools).run(vec![call]);
            block_on(joined.collect::<Vec<_>>())
                .pop()
                .expect("load result")
                .1
        };
        let again = load_again("hidden");
        assert!(again.ok);
        assert!(again.content.contains(r#""already_loaded":true"#));
        let unknown = load_again("missing");
        assert!(!unknown.ok);
        assert!(unknown.content.contains("unknown tool group"));
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
