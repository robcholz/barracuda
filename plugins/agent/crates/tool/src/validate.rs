use alloc::string::ToString;

use serde::de::IgnoredAny;

use super::definition::{ToolError, ToolInvokeError, ToolResult};

pub(super) fn normalize_arguments_json(arguments_json: &str) -> ToolResult<&str> {
    let text = normalized_arguments_json(arguments_json);
    serde_json::from_str::<IgnoredAny>(text).map_err(|error| {
        ToolInvokeError::new(ToolError::InvalidArgumentsJson(error.to_string()))
    })?;
    if !text.starts_with('{') {
        return Err(ToolInvokeError::new(ToolError::InvalidArgumentsJson(
            "tool arguments must be a JSON object".into(),
        )));
    }
    Ok(text)
}

fn normalized_arguments_json(arguments_json: &str) -> &str {
    let text = arguments_json.trim();
    if text.is_empty() {
        "{}"
    } else {
        text
    }
}
