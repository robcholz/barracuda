//! Native Agent Tool adapter for the Time capability.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, rc::Rc, string::String};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{EmptyArgs, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolOutput, ToolSpec},
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_time_plugin::{ClockError, UnixMillis, UtcClock};
use serde_json::json;
use time::OffsetDateTime;

/// Plugin that registers the Time capability as an Agent Tool.
#[barracuda_plugin::macros::plugin]
pub struct AgentTimePlugin;

impl AgentTimePlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentTimePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let clock = context.require::<UtcClock>("time")?;
        tools
            .register_group(ToolGroup::new(
                "time",
                true,
                [Tool::new(TimeNowTool { clock })],
            ))
            .map_err(PluginError::registration)
    }
}

struct TimeNowTool {
    clock: Rc<UtcClock>,
}

impl ToolSpec for TimeNowTool {
    barracuda_agent_plugin::tools::tool_metadata!("time_now");
}

impl ToolHandler for TimeNowTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let (response, ok) = match self.clock.now().and_then(format_utc) {
                Ok(utc) => (json!({ "utc": utc }), true),
                Err(error) => (json!({ "error": error.code() }), false),
            };
            let content = serde_json::to_string(&response).map_err(|_error| {
                ToolError::InvokeRejected(String::from("failed to encode Time response"))
            })?;
            Ok(ToolOutput { content, ok })
        })
    }
}

fn format_utc(timestamp: UnixMillis) -> Result<String, ClockError> {
    let unix_millis = u64::from(timestamp);
    let unix_seconds =
        i64::try_from(unix_millis / 1_000).map_err(|_error| ClockError::OutOfRange)?;
    let calendar = OffsetDateTime::from_unix_timestamp(unix_seconds)
        .map_err(|_error| ClockError::OutOfRange)?;
    if !(0..=9_999).contains(&calendar.year()) {
        return Err(ClockError::OutOfRange);
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        calendar.year(),
        u8::from(calendar.month()),
        calendar.day(),
        calendar.hour(),
        calendar.minute(),
        calendar.second(),
        unix_millis % 1_000,
    ))
}

#[cfg(test)]
mod tests {
    const VALIDATOR: json_validator::Validator =
        json_validator::validator!("resources/tools/time_now/schema.json");

    #[test]
    fn static_schema_accepts_only_an_empty_object() {
        assert!(VALIDATOR.validate_str("{}").is_ok());
        assert!(VALIDATOR.validate_str(r#"{"timezone":"local"}"#).is_err());
    }
}
