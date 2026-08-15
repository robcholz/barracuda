use alloc::{boxed::Box, string::ToString, sync::Arc};

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};

use super::super::model::TranscriptText;
use super::super::policy::SpawnPolicy;
use super::super::tool_port::SubagentControl;
use super::helper::trace_subagent_bound;
use super::spawn::{SpawnArgs, SpawnRequest};

pub(super) fn tool(control: Arc<SubagentControl>, policy: SpawnPolicy) -> Tool {
    Tool::new(RunSubagentTool { control, policy })
}

struct RunSubagentTool {
    control: Arc<SubagentControl>,
    policy: SpawnPolicy,
}

impl ToolSpec for RunSubagentTool {
    tool_metadata!("subagent_run");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new("subagent_run", RiskClass::Moderate)
    }
}

impl ToolHandler for RunSubagentTool {
    type Args = SpawnArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let request = SpawnRequest::from_args(args, &self.policy, "subagent_run")?;
            let (child, result) = self
                .control
                .spawn(
                    request.kind,
                    Some(request.name),
                    request.goal,
                    request.timeout,
                )
                .await
                .map_err(|error| ToolError::InvokeRejected(error.to_string()))?;
            trace_subagent_bound(child);
            let result = result.await.map_err(|_| {
                ToolError::InvokeRejected(format!("subagent {child} result channel closed"))
            })?;
            self.control.acknowledge_delivery(child);
            Ok(ToolOutput {
                content: result.text(),
                ok: result.ok(),
            })
        })
    }
}
