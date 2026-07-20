use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool identity retained only while its outcome is not durably settled.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct TrackedToolCall {
    tool: String,
    arguments: Value,
}

impl TrackedToolCall {
    pub(crate) fn new(tool: impl Into<String>, arguments: Value) -> Self {
        Self {
            tool: tool.into(),
            arguments,
        }
    }

    pub(crate) fn tool(&self) -> &str {
        &self.tool
    }

    pub(crate) fn arguments(&self) -> &Value {
        &self.arguments
    }
}
