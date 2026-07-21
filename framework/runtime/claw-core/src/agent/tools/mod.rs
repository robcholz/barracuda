//! Pure Agent tool groups.
//!
//! Tools owned by a context adapter stay beside that adapter. This module is
//! only for groups with no context-adapter domain owner. Orchestrator features
//! such as multiagent are injected as ordinary `ToolGroup`s during construction.
//!
//! Human approval is **not** a tool: it is raised by the permission layer (an
//! `Ask` decision in `base_agent`), not requested or resolved by the model.
//!
mod internal;

use claw_tool::ToolError;
use serde_json::Value;

pub(crate) use internal::internal_tools;

// -- Shared argument / rendering helpers -------------------------------------

/// Read one optional string argument out of a tool call.
///
/// # Errors
///
/// [`ToolError::InvalidArgumentsJson`] if the arguments are present but not valid JSON —
/// a malformed call is surfaced, not swallowed.
pub(crate) fn optional_string_argument(
    arguments_json: &str,
    key: &str,
) -> Result<Option<String>, ToolError> {
    let text = arguments_json.trim();
    let value = if text.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(text).map_err(|error| {
            ToolError::InvalidArgumentsJson(format!("invalid tool arguments JSON: {error}"))
        })?
    };
    if !value.is_object() {
        return Err(ToolError::InvalidArgumentsJson(
            "tool arguments must be a JSON object".into(),
        ));
    }
    match value.get(key) {
        Some(Value::String(text)) => Ok(Some(text.to_string())),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "'{key}' must be a string"
        ))),
        None => Ok(None),
    }
}
