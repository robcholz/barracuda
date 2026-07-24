use core::num::NonZeroU32;
use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, DetachedTool, DetachedToolFuture, DetachedToolHandler, Tool, ToolError,
    ToolInvocation, ToolInvokeError, ToolOutput, ToolSpec,
};

use crate::agent::tools::helper::non_blank_argument;
use crate::agent::AgentKind;
use crate::session::Message;
use serde_json::Value;

use super::super::model::{SubagentTimeout, TranscriptText};
use super::super::policy::SpawnPolicy;
use super::super::tool_port::SubagentControl;

pub(super) fn tool(control: Arc<SubagentControl>, policy: SpawnPolicy) -> Tool {
    Tool::from_detached(SpawnSubagentTool { control, policy })
}

struct SpawnSubagentTool {
    control: Arc<SubagentControl>,
    policy: SpawnPolicy,
}

impl ToolSpec for SpawnSubagentTool {
    tool_metadata!("subagent_spawn");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new("subagent_spawn", RiskClass::Moderate)
    }
}

impl DetachedToolHandler for SpawnSubagentTool {
    fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> DetachedToolFuture<'a> {
        Box::pin(async move { self.invoke_inner(call).await })
    }
}

impl SpawnSubagentTool {
    async fn invoke_inner(&self, call: &ToolInvocation) -> Result<DetachedTool, ToolInvokeError> {
        let request = SpawnRequest::parse(call, &self.policy, "subagent_spawn")?;
        let SpawnRequest {
            kind,
            name,
            goal,
            timeout,
        } = request;
        let (child, result) = self
            .control
            .spawn(kind, Some(name.clone()), goal, timeout)
            .await
            .map_err(|error| ToolError::InvokeRejected(error.to_string()))?;
        let accepted = ToolOutput {
            content: format!(
                "Subagent {child} named '{name}' started with a {} ms timeout; its result will be delivered automatically.",
                timeout.millis()
            ),
            ok: true,
        };
        let control = Arc::clone(&self.control);
        let completion = Box::pin(async move {
            let result = result.recv().await.map_err(|_| {
                ToolError::InvokeRejected("subagent result channel closed".to_owned())
            })?;
            control.acknowledge_delivery(child);
            Ok(ToolOutput {
                content: result.text(),
                ok: result.ok(),
            })
        });
        Ok(DetachedTool::new(accepted, completion))
    }
}

pub(super) struct SpawnRequest {
    pub(super) kind: AgentKind,
    pub(super) name: String,
    pub(super) goal: Message,
    pub(super) timeout: SubagentTimeout,
}

impl SpawnRequest {
    pub(super) fn parse(
        call: &ToolInvocation,
        policy: &SpawnPolicy,
        tool_name: &str,
    ) -> Result<Self, ToolInvokeError> {
        let args = call.arguments_value()?;
        let kind = AgentKind::new(non_blank_argument(&args, "kind")?);
        validate_kind(policy, &kind, tool_name)?;
        Ok(Self {
            kind,
            name: non_blank_argument(&args, "name")?,
            goal: Message::text(non_blank_argument(&args, "goal")?),
            timeout: SubagentTimeout::new(required_nonzero_u32_argument(&args, "timeout_ms")?),
        })
    }
}

fn validate_kind(
    policy: &SpawnPolicy,
    kind: &AgentKind,
    tool_name: &str,
) -> Result<(), ToolInvokeError> {
    if !policy.allows(kind) {
        tracing::warn!(name: "spawn_kind_rejected", kind = %kind.as_str());
        return Err(ToolError::InvokeRejected(format!(
            "{tool_name}: kind '{kind}' is not permitted for this agent. Allowed: {}",
            policy.describe()
        ))
        .into());
    }
    if SpawnPolicy::is_known(kind) {
        return Ok(());
    }

    tracing::warn!(name: "spawn_unknown_kind_rejected", kind = %kind.as_str());
    let available = policy
        .catalog()
        .iter()
        .map(|(agent_kind, _)| agent_kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let available = if available.is_empty() {
        "(none)".to_owned()
    } else {
        available
    };
    Err(ToolError::InvokeRejected(format!(
        "{tool_name}: '{kind}' is not a known agent kind. Spawnable kinds: {available}"
    ))
    .into())
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
#[allow(clippy::expect_used)]
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
