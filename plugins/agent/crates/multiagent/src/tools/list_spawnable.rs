use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::policy::SpawnPolicy;

pub(super) fn tool(policy: SpawnPolicy) -> Tool {
    Tool::new(ListSpawnableAgentsTool { policy })
}

struct ListSpawnableAgentsTool {
    policy: SpawnPolicy,
}

impl ToolSpec for ListSpawnableAgentsTool {
    tool_metadata!("subagent_list_spawnable");

    /// Only reads state.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }

    fn concurrent(&self) -> bool {
        true
    }
}

impl ToolHandler for ListSpawnableAgentsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let kinds: Vec<serde_json::Value> = self
                .policy
                .catalog()
                .iter()
                .map(|(kind, description)| {
                    serde_json::json!({ "kind": kind.as_str(), "description": description })
                })
                .collect();
            Ok(ToolOutput {
                content: serde_json::json!({ "spawnable_agents": kinds }).to_string(),
                ok: true,
            })
        })
    }
}
use alloc::{string::ToString, vec::Vec};
