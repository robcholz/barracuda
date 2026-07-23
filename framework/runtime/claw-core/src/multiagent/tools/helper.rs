use claw_permission::{Action, Resource, RiskClass};
use claw_tool::{ToolError, ToolInvocation, ToolInvokeError};
use serde_json::Value;

use crate::agent::tools::helper::optional_string_argument;
use crate::agent::AgentId;

pub(super) fn required_agent_id(args: &Value, tool: &str) -> Result<AgentId, ToolInvokeError> {
    let raw = optional_string_argument(args, "agent")?
        .ok_or_else(|| ToolError::InvalidArguments(format!("{tool} 'agent' is required")))?;
    let agent = raw.trim();
    if agent.is_empty() {
        return Err(ToolError::InvalidArguments(format!("{tool} 'agent' is required")).into());
    }
    AgentId::from_wire(agent)
        .map_err(|error| ToolError::InvokeRejected(format!("invalid agent id '{agent}': {error}")))
        .map_err(Into::into)
}

pub(super) fn action_with_agent_resource(
    name: &'static str,
    risk: RiskClass,
    call: &ToolInvocation,
) -> Action {
    let action = Action::new(name, risk);
    let Some(resource) = call
        .arguments_value()
        .ok()
        .and_then(|args| agent_resource(&args))
    else {
        return action;
    };
    action.with_resource(resource)
}

fn agent_resource(args: &Value) -> Option<Resource> {
    let raw = optional_string_argument(args, "agent").ok().flatten()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| Resource::Agent(trimmed.to_string()))
}
