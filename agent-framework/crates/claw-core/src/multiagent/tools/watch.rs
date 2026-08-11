use alloc::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, Tool, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::tool_port::SubagentControl;
use super::helper::{action_with_agent_resource, required_agent_id};

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
    fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let args = call.arguments_value()?;
            let target = required_agent_id(&args, "subagent_watch")?;
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
