mod delete;
mod followup;
mod helper;
mod list;
mod list_spawnable;
mod spawn;
mod watch;

use std::sync::Arc;

use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};

use super::policy::SpawnPolicy;
use super::tool_port::{MultiagentBridge, SubagentControl};

/// Build the complete multiagent tool extension for one agent. `None` means
/// the catalog does not grant that agent spawning capabilities.
pub(crate) fn tool_group(
    caller: AgentId,
    kind: &AgentKind,
    bridge: Arc<MultiagentBridge>,
) -> Option<ToolGroup> {
    let policy = SpawnPolicy::for_agent(kind)?;
    let control = Arc::new(SubagentControl::new(caller, bridge));
    Some(ToolGroup::new(
        "subagent",
        true,
        [
            list_spawnable::tool(policy.clone()),
            spawn::tool(Arc::clone(&control), policy),
            list::tool(Arc::clone(&control)),
            watch::tool(Arc::clone(&control)),
            delete::tool(Arc::clone(&control)),
            followup::tool(control),
        ],
    ))
}
