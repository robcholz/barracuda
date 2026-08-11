use alloc::{string::ToString, sync::Arc};

use claw_tool::{tool_metadata, EmptyArgs, Tool, ToolFuture, ToolHandler, ToolOutput, ToolSpec};

use super::super::tool_port::SubagentControl;

pub(super) fn tool(control: Arc<SubagentControl>) -> Tool {
    Tool::new(ListSubagentsTool { control })
}

struct ListSubagentsTool {
    control: Arc<SubagentControl>,
}

impl ToolSpec for ListSubagentsTool {
    tool_metadata!("subagent_list");

    fn concurrent(&self) -> bool {
        true
    }
}

impl ToolHandler for ListSubagentsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            Ok(ToolOutput {
                content: serde_json::json!({ "subagents": self.control.list() }).to_string(),
                ok: true,
            })
        })
    }
}
