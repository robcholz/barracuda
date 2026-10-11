use alloc::string::ToString;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, Tool, ToolContext, ToolFuture, ToolHandler, ToolInvocation,
    ToolOutput, ToolSpec,
};
use portable_atomic_util::Arc;

use super::super::tool_port::SubagentControl;

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::new(ListSubagentsTool { control })
}

struct ListSubagentsTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for ListSubagentsTool {
    tool_metadata!("subagent_list");

    /// Only reads state.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }

    fn concurrent(&self) -> bool {
        true
    }
}

impl ToolHandler for ListSubagentsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _context: ToolContext, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            Ok(ToolOutput::new(
                serde_json::json!({ "subagents": self.control.list() }).to_string(),
                true,
            ))
        })
    }
}
