use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, Tool, ToolFuture, ToolHandler, ToolOutput, ToolSpec,
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

    fn concurrent(&self) -> bool {
        true
    }
}

impl ToolHandler for ListSpawnableAgentsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        _args: Self::Args,
    ) -> ToolFuture<'a> {
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
