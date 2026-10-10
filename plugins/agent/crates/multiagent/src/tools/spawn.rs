use alloc::{borrow::ToOwned, boxed::Box, string::String, string::ToString, vec::Vec};
use core::num::NonZeroU32;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, BackgroundTool, BackgroundToolControl, BackgroundToolFuture,
    BackgroundToolHandler, Tool, ToolError, ToolFuture, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};
use portable_atomic_util::Arc;
use serde::Deserialize;

use barracuda_agent::Message;
use barracuda_agent::{AgentId, AgentKind};

use super::super::model::{SubagentTimeout, TranscriptText};
use super::super::policy::SpawnPolicy;
use super::super::tool_port::SubagentControl;
use super::helper::trace_subagent_bound;

pub(super) fn tool(control: Arc<SubagentControl>, policy: SpawnPolicy) -> Tool {
    Tool::background(SpawnSubagentTool { control, policy })
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

impl BackgroundToolHandler for SpawnSubagentTool {
    type Args = SpawnArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> BackgroundToolFuture<'a> {
        Box::pin(async move { self.invoke_inner(args).await })
    }
}

impl SpawnSubagentTool {
    async fn invoke_inner(&self, args: SpawnArgs) -> Result<BackgroundTool, ToolInvokeError> {
        let request = SpawnRequest::from_args(args, &self.policy, "subagent_spawn")?;
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
        trace_subagent_bound(child);
        let accepted = ToolOutput {
            content: format!(
                "Subagent {child} named '{name}' started with a {} ms timeout; its result will be delivered automatically.",
                timeout.millis()
            ),
            ok: true,
        };
        let control = Arc::clone(&self.control);
        let completion = Box::pin(async move {
            let result = result.await.map_err(|_| {
                ToolError::InvokeRejected("subagent result channel closed".to_owned())
            })?;
            control.acknowledge_delivery(child);
            Ok(ToolOutput {
                content: result.text(),
                ok: result.ok(),
            })
        });
        Ok(
            BackgroundTool::new(accepted, completion).with_control(SpawnedSubagent {
                control: Arc::clone(&self.control),
                child,
            }),
        )
    }
}

/// Background-pool control over one spawned child: input interrupts it with a
/// new task, and cancelling deletes it with its subtree.
struct SpawnedSubagent {
    control: Arc<SubagentControl>,
    child: AgentId,
}

impl BackgroundToolControl for SpawnedSubagent {
    fn status(&self) -> Option<String> {
        let snapshot = self.control.get(self.child)?;
        serde_json::to_string(&snapshot).ok()
    }

    fn input<'a>(&'a self, input: Option<String>) -> ToolFuture<'a> {
        Box::pin(async move {
            let Some(input) = input else {
                return Err(ToolError::InvokeRejected(
                    "a subagent takes a message as input, not end of input".to_owned(),
                )
                .into());
            };
            let child = self.child;
            let message = Message::text(input.trim().to_owned());
            Ok(match self.control.followup(child, message).await {
                Ok(()) => ToolOutput {
                    content: format!("Subagent {child} interrupted and sent the new input."),
                    ok: true,
                },
                Err(error) => ToolOutput {
                    content: format!("Cannot interrupt {child}: {error}."),
                    ok: false,
                },
            })
        })
    }

    fn cancel(&self) {
        self.control.request_delete(self.child);
    }
}

pub(super) struct SpawnRequest {
    pub(super) kind: AgentKind,
    pub(super) name: String,
    pub(super) goal: Message,
    pub(super) timeout: SubagentTimeout,
}

#[derive(Deserialize)]
pub(super) struct SpawnArgs {
    kind: String,
    name: String,
    goal: String,
    timeout_ms: u32,
}

impl SpawnRequest {
    pub(super) fn from_args(
        args: SpawnArgs,
        policy: &SpawnPolicy,
        tool_name: &str,
    ) -> Result<Self, ToolInvokeError> {
        let kind = AgentKind::new(trimmed(args.kind));
        validate_kind(policy, &kind, tool_name)?;
        let timeout_ms = NonZeroU32::new(args.timeout_ms).ok_or_else(|| {
            ToolError::InvalidArguments("'timeout_ms' must be greater than zero".to_owned())
        })?;
        Ok(Self {
            kind,
            name: trimmed(args.name),
            goal: Message::text(trimmed(args.goal)),
            timeout: SubagentTimeout::new(timeout_ms),
        })
    }
}

fn validate_kind(
    policy: &SpawnPolicy,
    kind: &AgentKind,
    tool_name: &str,
) -> Result<(), ToolInvokeError> {
    if !policy.allows(kind) {
        log::warn!("subagent spawn kind rejected by policy: {}", kind.as_str());
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

    log::warn!("unknown subagent spawn kind rejected: {}", kind.as_str());
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

fn trimmed(value: String) -> String {
    value.trim().to_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_agent_tool::ToolError;

    use super::{SpawnArgs, SpawnRequest};
    use crate::policy::SpawnPolicy;

    #[test]
    fn zero_timeout_is_a_spawn_domain_error() {
        let args = SpawnArgs {
            kind: "worker".into(),
            name: "x".into(),
            goal: "y".into(),
            timeout_ms: 0,
        };
        assert!(matches!(
            SpawnRequest::from_args(args, &SpawnPolicy::Any, "subagent_spawn"),
            Err(error) if matches!(error.error, ToolError::InvalidArguments(_))
        ));
    }
}
