use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, AsyncToolHandler, Tool, ToolError, ToolFuture, ToolInvocation, ToolOutput,
    ToolSpec,
};

use super::super::model::TranscriptText;
use super::super::policy::SpawnPolicy;
use super::super::tool_port::SubagentControl;
use super::spawn::SpawnRequest;

pub(super) fn tool(control: Arc<SubagentControl>, policy: SpawnPolicy) -> Tool {
    Tool::from_async(RunSubagentTool { control, policy })
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

impl AsyncToolHandler for RunSubagentTool {
    fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a> {
        Box::pin(async move {
            let request = SpawnRequest::parse(call, &self.policy, "subagent_run")?;
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
            let result = result.recv().await.map_err(|_| {
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
