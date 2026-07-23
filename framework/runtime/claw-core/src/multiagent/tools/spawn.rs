use core::num::NonZeroU32;
use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, AsyncToolHandler, Tool, ToolError, ToolFuture, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};

use crate::agent::tools::helper::non_blank_argument;
use crate::agent::AgentKind;
use crate::session::Message;
use claw_api::ToolCall;
use serde_json::Value;

use super::super::model::{SubagentTimeout, TranscriptText};
use super::super::policy::SpawnPolicy;
use super::super::tool_port::SubagentControl;

pub(super) fn tool(control: Arc<SubagentControl>, policy: SpawnPolicy) -> Tool {
    Tool::from_async(SpawnSubagentTool { control, policy })
}

struct SpawnSubagentTool {
    control: Arc<SubagentControl>,
    policy: SpawnPolicy,
}

impl ToolSpec for SpawnSubagentTool {
    tool_metadata!("subagent_spawn");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new("subagent_spawn", RiskClass::Moderate)
    }
}

impl AsyncToolHandler for SpawnSubagentTool {
    fn invoke<'a>(&'a self, call: &'a ToolInvocation<'_>) -> ToolFuture<'a> {
        Box::pin(async move { self.invoke_inner(call).await })
    }
}

impl SpawnSubagentTool {
    async fn invoke_inner(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let args = call.arguments_value()?;
        let kind = AgentKind::new(non_blank_argument(&args, "kind")?);
        if !self.policy.allows(&kind) {
            tracing::warn!(name: "spawn_kind_rejected", kind = %kind.as_str());
            return Ok(ToolOutput {
                output: format!(
                    "subagent_spawn: kind '{kind}' is not permitted for this agent. \
                     Allowed: {}. This is a policy restriction, not a transient error: \
                     pick a permitted kind or handle the work yourself.",
                    self.policy.describe()
                ),
                ok: false,
            });
        }

        if !SpawnPolicy::is_known(&kind) {
            tracing::warn!(name: "spawn_unknown_kind_rejected", kind = %kind.as_str());
            let available = self
                .policy
                .catalog()
                .iter()
                .map(|(agent_kind, _)| agent_kind.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let available = if available.is_empty() {
                "(none)".to_string()
            } else {
                available
            };
            return Ok(ToolOutput {
                output: format!(
                    "subagent_spawn: '{kind}' is not a known agent kind, so it cannot be \
                     created. Spawnable kinds: {available}. Call subagent_list_spawnable to see \
                     what you can spawn."
                ),
                ok: false,
            });
        }

        let name = non_blank_argument(&args, "name")?;
        let goal = Message::text(non_blank_argument(&args, "goal")?);
        let foreground = required_bool_argument(&args, "foreground")?;
        let timeout = SubagentTimeout::new(required_nonzero_u32_argument(&args, "timeout_ms")?);
        if foreground {
            let (_child, result) = match self
                .control
                .spawn_foreground(kind, Some(name), goal, timeout)
                .await
            {
                Ok(spawn) => spawn,
                Err(message) => {
                    return Ok(ToolOutput {
                        output: message,
                        ok: false,
                    });
                }
            };
            let result = result.recv().await.map_err(|_| {
                ToolError::InvokeRejected("foreground subagent result channel closed".to_owned())
            })?;
            Ok(ToolOutput {
                output: result.text(),
                ok: result.ok(),
            })
        } else {
            let source_call = ToolCall {
                id: call.id().unwrap_or_default().to_owned(),
                name: call.name().to_owned(),
                arguments_json: call.arguments_json().to_owned(),
            };
            let child = match self
                .control
                .spawn_background(kind, Some(name.clone()), goal, timeout, source_call)
                .await
            {
                Ok(child) => child,
                Err(message) => {
                    return Ok(ToolOutput {
                        output: message,
                        ok: false,
                    });
                }
            };
            Ok(ToolOutput {
                output: format!(
                    "Subagent {child} named '{name}' requested with a {} ms timeout; its result will be reported back when it finishes.",
                    timeout.millis()
                ),
                ok: true,
            })
        }
    }
}

fn required_bool_argument(args: &Value, key: &str) -> Result<bool, ToolError> {
    match args.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "'{key}' must be a boolean"
        ))),
        None => Err(ToolError::InvalidArguments(format!("'{key}' is required"))),
    }
}

fn required_nonzero_u32_argument(args: &Value, key: &str) -> Result<NonZeroU32, ToolError> {
    let Some(raw) = args.get(key) else {
        return Err(ToolError::InvalidArguments(format!("'{key}' is required")));
    };
    let Some(raw) = raw.as_u64() else {
        return Err(ToolError::InvalidArguments(format!(
            "'{key}' must be a positive integer"
        )));
    };
    u32::try_from(raw)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!("'{key}' must be between 1 and {}", u32::MAX))
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::required_nonzero_u32_argument;

    #[test]
    fn required_nonzero_u32_rejects_missing_zero_negative_fractional_and_oversized_values() {
        assert_eq!(
            required_nonzero_u32_argument(&json!({"timeout_ms": 2500}), "timeout_ms")
                .expect("valid timeout")
                .get(),
            2_500
        );

        for arguments in [
            json!({}),
            json!({"timeout_ms": 0}),
            json!({"timeout_ms": -1}),
            json!({"timeout_ms": 1.5}),
            json!({"timeout_ms": 4_294_967_296_u64}),
            json!({"timeout_ms": "1000"}),
        ] {
            assert!(
                required_nonzero_u32_argument(&arguments, "timeout_ms").is_err(),
                "unexpectedly accepted {arguments}"
            );
        }
    }
}
