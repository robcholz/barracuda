use alloc::{boxed::Box, sync::Arc};

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id, AgentArgs};

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::new(DeleteSubagentTool { control })
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

impl ToolHandler for DeleteSubagentTool {
    type Args = AgentArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let target = required_agent_id(args.agent)?;
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
