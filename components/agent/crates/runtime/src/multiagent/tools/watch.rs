use alloc::sync::Arc;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id, AgentArgs};

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::new(WatchSubagentTool { control })
}

struct WatchSubagentTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for WatchSubagentTool {
    tool_metadata!("subagent_watch");

    fn concurrent(&self) -> bool {
        true
    }

    fn classify(&self, call: &ToolInvocation) -> Action {
        action_with_agent_resource("subagent_watch", RiskClass::Safe, call)
    }
}

impl ToolHandler for WatchSubagentTool {
    type Args = AgentArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let target = required_agent_id(args.agent)?;
            match self.control.get(target) {
                Some(snapshot) => Ok(ToolOutput {
                    content: serde_json::to_string(&snapshot).map_err(|error| {
                        ToolError::InvokeRejected(format!(
                            "failed to serialize subagent snapshot: {error}"
                        ))
                    })?,
                    ok: true,
                }),
                None => Ok(ToolOutput {
                    content: format!(
                        "No subagent {target} in your subtree (unknown id, or not one of yours)."
                    ),
                    ok: false,
                }),
            }
        })
    }
}
