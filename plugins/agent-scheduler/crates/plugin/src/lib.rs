//! Native Agent Tool adapter for the Scheduler capability.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_scheduler_plugin::{CancelRequest, ScheduleError, ScheduleRequest, Scheduler};
use serde::Serialize;

/// Plugin that registers Scheduler operations as Agent Tools.
#[barracuda_plugin::macros::plugin]
pub struct AgentSchedulerPlugin;

impl AgentSchedulerPlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentSchedulerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let scheduler = context.require::<Scheduler>("scheduler")?;
        tools
            .register_group(ToolGroup::new(
                "scheduler",
                true,
                [
                    Tool::new(ScheduleTool {
                        scheduler: Rc::clone(&scheduler),
                    }),
                    Tool::new(CancelTool { scheduler }),
                ],
            ))
            .map_err(PluginError::registration)
    }
}

struct ScheduleTool {
    scheduler: Rc<Scheduler>,
}

impl ToolSpec for ScheduleTool {
    barracuda_agent_plugin::tools::tool_metadata!("scheduler_schedule");
}

impl ToolHandler for ScheduleTool {
    type Args = ScheduleRequest;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.scheduler.schedule(args).await) })
    }
}

struct CancelTool {
    scheduler: Rc<Scheduler>,
}

impl ToolSpec for CancelTool {
    barracuda_agent_plugin::tools::tool_metadata!("scheduler_cancel");
}

impl ToolHandler for CancelTool {
    type Args = CancelRequest;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.scheduler.cancel(args).await) })
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

fn tool_output<Response: Serialize>(
    result: Result<Response, ScheduleError>,
) -> Result<ToolOutput, ToolInvokeError> {
    let (content, ok) = match result {
        Ok(response) => (serde_json::to_string(&response), true),
        Err(error) => (
            serde_json::to_string(&ErrorResponse {
                error: error.code(),
            }),
            false,
        ),
    };
    let content = content.map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode Scheduler response"))
    })?;
    Ok(ToolOutput { content, ok })
}

#[cfg(test)]
mod tests {
    const SCHEDULE: json_validator::Validator =
        json_validator::validator!("resources/tools/scheduler_schedule/schema.json");
    const CANCEL: json_validator::Validator =
        json_validator::validator!("resources/tools/scheduler_cancel/schema.json");

    #[test]
    fn static_schemas_preserve_the_scheduler_request_contracts() {
        assert!(
            SCHEDULE
                .validate_str(
                    r#"{"id":"meeting","trigger":{"type":"once","at":"2027-01-15T08:30:00.000Z"}}"#,
                )
                .is_ok()
        );
        assert!(SCHEDULE.validate_str(r#"{"id":"meeting"}"#).is_err());
        assert!(CANCEL.validate_str(r#"{"id":"meeting"}"#).is_ok());
        assert!(
            CANCEL
                .validate_str(r#"{"id":"meeting","force":true}"#)
                .is_err()
        );
    }
}
