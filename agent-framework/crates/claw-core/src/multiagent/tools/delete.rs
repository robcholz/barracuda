use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, AsyncToolHandler, Tool, ToolFuture, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id};

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::from_async(DeleteSubagentTool { control })
}

struct DeleteSubagentTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for DeleteSubagentTool {
    tool_metadata!("subagent_delete");

    fn classify(&self, call: &ToolInvocation) -> Action {
        action_with_agent_resource("subagent_delete", RiskClass::High, call)
    }
}

impl AsyncToolHandler for DeleteSubagentTool {
    fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments_value()?;
            let target = required_agent_id(&args, "subagent_delete")?;
            if self.control.get(target).is_none() {
                return Ok(ToolOutput {
                    content: format!(
                        "Cannot delete {target}: it is not a subagent in your subtree."
                    ),
                    ok: false,
                });
            }
            match self.control.delete(target).await {
                Ok(()) => Ok(ToolOutput {
                    content: format!("Subagent {target} and its subtree deleted."),
                    ok: true,
                }),
                Err(error) => Ok(ToolOutput {
                    content: format!("Cannot delete {target}: {error}."),
                    ok: false,
                }),
            }
        })
    }
}
