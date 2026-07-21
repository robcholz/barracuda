//! The `tool_discovery` group: search hidden groups and load one for the next turn.

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolDiscoveryHandle, ToolError, ToolGroup,
    ToolInvocation, ToolInvokeError, ToolOutput, ToolSpec,
};
use serde_json::json;

use super::optional_string_argument;

/// Build the always-visible discovery group over a [`ToolSet`](claw_tool::ToolSet)
/// bridge. All other registered groups remain hidden until `tool_load` reveals
/// one for the next turn.
pub(crate) fn discovery_tools(discovery: ToolDiscoveryHandle) -> ToolGroup {
    ToolGroup::new(
        "tool_discovery",
        true,
        [
            Tool::from_sync(ToolSearchTool {
                discovery: discovery.clone(),
            }),
            Tool::from_sync(ToolLoadTool { discovery }),
        ],
    )
}

/// Reads the owning [`ToolSet`](claw_tool::ToolSet)'s loadable catalog and
/// returns it — group ids, tool names, and short descriptions, never schemas.
struct ToolSearchTool {
    discovery: ToolDiscoveryHandle,
}

/// Queues a group to be enabled on the next Agent tick.
struct ToolLoadTool {
    discovery: ToolDiscoveryHandle,
}

impl ToolSpec for ToolLoadTool {
    tool_metadata!("tool_load");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for ToolLoadTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let group_id = optional_string_argument(call.arguments_json(), "group_id")?
            .map(|group_id| group_id.trim().to_owned())
            .filter(|group_id| !group_id.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidArguments("tool_load 'group_id' is required".into())
            })?;

        let loaded = self.discovery.request_load(group_id.clone());
        Ok(ToolOutput {
            output: json!({
                "group_id": group_id,
                "loaded": loaded,
                "available_next_turn": loaded,
            })
            .to_string(),
            ok: loaded,
        })
    }
}

impl ToolSpec for ToolSearchTool {
    tool_metadata!("tool_search");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for ToolSearchTool {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        Ok(ToolOutput {
            output: json!({ "tool_groups": self.discovery.catalog() }).to_string(),
            ok: true,
        })
    }
}
