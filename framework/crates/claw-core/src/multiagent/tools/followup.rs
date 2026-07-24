use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, AsyncToolHandler, Tool, ToolFuture, ToolInvocation, ToolOutput, ToolSpec,
};

use crate::agent::tools::helper::non_blank_argument;
use crate::session::Message;

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id};

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::from_async(FollowupSubagentTool { control })
}

struct FollowupSubagentTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for FollowupSubagentTool {
    tool_metadata!("subagent_followup");

    fn classify(&self, call: &ToolInvocation) -> Action {
        action_with_agent_resource("subagent_followup", RiskClass::Moderate, call)
    }
}

impl AsyncToolHandler for FollowupSubagentTool {
    fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments_value()?;
            let target = required_agent_id(&args, "subagent_followup")?;
            let message = Message::text(non_blank_argument(&args, "message")?);
            if self.control.get(target).is_none() {
                return Ok(ToolOutput {
                    content: format!(
                        "Cannot follow up {target}: it is not a subagent in your subtree."
                    ),
                    ok: false,
                });
            }
            match self.control.followup(target, message).await {
                Ok(()) => Ok(ToolOutput {
                    content: format!("Subagent {target} retasked with new input."),
                    ok: true,
                }),
                Err(error) => Ok(ToolOutput {
                    content: format!("Cannot follow up {target}: {error}."),
                    ok: false,
                }),
            }
        })
    }
}
