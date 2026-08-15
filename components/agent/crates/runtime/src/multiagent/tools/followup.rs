use alloc::{borrow::ToOwned, boxed::Box, string::String, sync::Arc};

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};
use serde::Deserialize;

use crate::Message;

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id};

#[derive(Deserialize)]
struct FollowupArgs {
    agent: String,
    message: String,
}

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::new(FollowupSubagentTool { control })
}

struct FollowupSubagentTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for FollowupSubagentTool {
    tool_metadata!("subagent_interrupt");

    fn classify(&self, call: &ToolInvocation) -> Action {
        action_with_agent_resource("subagent_interrupt", RiskClass::Moderate, call)
    }
}

impl ToolHandler for FollowupSubagentTool {
    type Args = FollowupArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let target = required_agent_id(args.agent)?;
            let message = Message::text(args.message.trim().to_owned());
            if self.control.get(target).is_none() {
                return Ok(ToolOutput {
                    content: format!(
                        "Cannot interrupt {target}: it is not a subagent in your subtree."
                    ),
                    ok: false,
                });
            }
            match self.control.followup(target, message).await {
                Ok(()) => Ok(ToolOutput {
                    content: format!("Subagent {target} interrupted and sent the new input."),
                    ok: true,
                }),
                Err(error) => Ok(ToolOutput {
                    content: format!("Cannot interrupt {target}: {error}."),
                    ok: false,
                }),
            }
        })
    }
}
