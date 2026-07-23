//! Helpers shared by tools from different Agent submodules.

use claw_tool::ToolError;
use serde_json::Value;

pub(crate) fn optional_string_argument(
    args: &Value,
    key: &str,
) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        Some(Value::String(text)) => Ok(Some(text.to_string())),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "'{key}' must be a string"
        ))),
        None => Ok(None),
    }
}

pub(crate) fn non_blank_argument(args: &Value, key: &str) -> Result<String, ToolError> {
    let Some(raw) = optional_string_argument(args, key)? else {
        return Err(ToolError::InvalidArguments(format!("'{key}' is required")));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvokeRejected(format!(
            "'{key}' must not be blank"
        )));
    }
    Ok(trimmed.to_string())
}
