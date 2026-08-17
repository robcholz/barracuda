use barracuda_agent_permission::{Action, Resource, RiskClass};
use barracuda_agent_tool::{ToolError, ToolInvocation, ToolInvokeError};
use serde::Deserialize;

use barracuda_agent::AgentId;

#[derive(Deserialize)]
pub(super) struct AgentArgs {
    pub(super) agent: String,
}

#[derive(Deserialize)]
struct AgentField {
    agent: String,
}

pub(super) fn trace_subagent_bound(child: AgentId) {
    tracing::event!(
        name: "flow_link",
        tracing::Level::INFO,
        {
            "flow.name" = "subagent",
            "flow.target_task" = %child,
            "flow.target_span" = "agent",
            "flow.arg.child_agent" = %child,
        }
    );
}

pub(super) fn required_agent_id(raw: String) -> Result<AgentId, ToolInvokeError> {
    let agent = raw.trim();
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
    let Ok(args) = call.arguments::<AgentField>() else {
        return action;
    };
    let agent = args.agent.trim();
    action.with_resource(Resource::Agent(agent.to_string()))
}
use alloc::string::{String, ToString};
