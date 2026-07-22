//! Internal natural-language approval resolver for one session actor.
//!
//! This is deliberately not an agent tool. The channel user replies in free
//! text, and the orchestrator runs one short LLM/tool round to classify that text
//! into the internal [`ApprovalDecision`] it feeds back to the parked agent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use claw_api::{ChatError, ChatRequest, ClawApiAsync, InitError, RetryPolicy, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, RawToolInvocation, SyncToolHandler, Tool, ToolError, ToolExecutor, ToolGroup,
    ToolInvocation, ToolInvokeError, ToolOutput, ToolRegistry, ToolSetError, ToolSpec,
};
use serde_json::{json, Value};

use crate::agent::ApprovalDecision;
use crate::config::{ApiUsage, SharedApiManager};
use crate::multiagent::DriveControl;

const APPROVAL_RESOLVER_PROMPT: &str = prompt!("approval/resolver_system.md");

const DEFAULT_CLARIFICATION: &str = "Please clearly reply with approval or rejection.";
const DEFAULT_REJECTION: &str = "rejected";
static APPROVAL_TOOL_PARENT: LazyLock<Arc<ToolRegistry>> =
    LazyLock::new(|| Arc::new(ToolRegistry::new()));

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PermissionReplyResolution {
    Approved,
    Rejected(String),
    Clarify(String),
}

impl PermissionReplyResolution {
    pub(crate) fn into_decision(self) -> Option<ApprovalDecision> {
        match self {
            Self::Approved => Some(ApprovalDecision::Approved),
            Self::Rejected(reason) => Some(ApprovalDecision::Rejected(reason)),
            Self::Clarify(_) => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ApprovalResolverError {
    #[error("approval resolver was cancelled")]
    Cancelled,
    #[error("failed to initialize approval resolver LLM: {0}")]
    Init(#[from] InitError),
    #[error(transparent)]
    ToolSet(#[from] ToolSetError),
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("approval resolver returned a malformed tool call")]
    MalformedToolCall,
}

struct ApprovalResolverControl {
    interrupt: Arc<AtomicBool>,
}

impl ApprovalResolverControl {
    fn new() -> Self {
        Self {
            interrupt: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt)
    }
}

struct ResolvePermissionReplyTool {
    resolution: Arc<Mutex<Option<PermissionReplyResolution>>>,
}

impl ResolvePermissionReplyTool {
    fn new(resolution: Arc<Mutex<Option<PermissionReplyResolution>>>) -> Self {
        Self { resolution }
    }
}

impl ToolSpec for ResolvePermissionReplyTool {
    tool_metadata!("permission_resolve_reply");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new("permission_resolve_reply", RiskClass::Safe)
    }
}

impl SyncToolHandler for ResolvePermissionReplyTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let args: Value = serde_json::from_str(call.arguments_json()).map_err(|error| {
            ToolError::InvalidArgumentsJson(format!("invalid tool arguments JSON: {error}"))
        })?;
        if !args.is_object() {
            return Err(ToolError::InvalidArgumentsJson(
                "tool arguments must be a JSON object".into(),
            )
            .into());
        }

        let Some(decision) = args
            .get("decision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(ToolError::InvalidArguments(
                "decision is required and must be a non-empty string".into(),
            )
            .into());
        };

        let resolution = match decision {
            "approve" => PermissionReplyResolution::Approved,
            "reject" => PermissionReplyResolution::Rejected(
                match args
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(note) => note,
                    None => DEFAULT_REJECTION,
                }
                .to_string(),
            ),
            "clarify" => PermissionReplyResolution::Clarify(
                match args
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(message) => message,
                    None => DEFAULT_CLARIFICATION,
                }
                .to_string(),
            ),
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "decision must be approve|reject|clarify, got '{other}'"
                ))
                .into())
            }
        };

        *self
            .resolution
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(resolution);
        Ok(ToolOutput {
            output: "approval reply resolved".to_string(),
            ok: true,
        })
    }
}

pub(crate) async fn resolve_permission_reply<H, Timer>(
    api_manager: &SharedApiManager,
    tool_call: &ToolCall,
    reason: &str,
    user_reply: &str,
    control: &DriveControl,
) -> Result<PermissionReplyResolution, ApprovalResolverError>
where
    H: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let mut llm = ClawApiAsync::<H, Timer>::new(H::default(), Timer::default());
    // Approval resolution runs on the root agent's config (its explicit binding,
    // else the default). With no binding, the request reports NotConfigured.
    if let Some(config) = api_manager
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_api(ApiUsage::RootAgent)
    {
        llm.set_config(config)?;
    }
    let resolution = Arc::new(Mutex::new(None));
    // Approval classification uses an isolated local tool set.
    let mut tools = APPROVAL_TOOL_PARENT.tool_set();
    tools.add_group(ToolGroup::new(
        "permission",
        true,
        [Tool::from_sync(ResolvePermissionReplyTool::new(
            Arc::clone(&resolution),
        ))],
    ))?;
    let tools = tools.begin()?;
    let resolver_control = ApprovalResolverControl::new();
    let cancel_handle = resolver_control.cancel_handle();
    control.set_cancel_hook(move || {
        cancel_handle.store(true, Ordering::Release);
    });

    let messages = json!([
        {
            "role": "user",
            "content": format!(
                "Pending tool call:\nID: {}\nName: {}\nArguments JSON: {}\n\nPermission reason:\n{reason}\n\nUser reply:\n{user_reply}",
                tool_call.id,
                tool_call.name,
                tool_call.arguments_json,
            )
        }
    ]);
    let request = ChatRequest {
        system_prompt: APPROVAL_RESOLVER_PROMPT,
        messages: &messages,
        reminders: &[],
        tools_json: Some(tools.schemas_json()),
        retry: RetryPolicy::none(),
    };
    let response = llm
        .chat(&request, Cancel::new(resolver_control.interrupt.as_ref()))
        .await;
    control.clear_cancel_hook();

    let response = match response {
        Ok(response) => response,
        Err(error) if resolver_control.interrupt.load(Ordering::Acquire) || error.is_aborted() => {
            return Err(ApprovalResolverError::Cancelled)
        }
        Err(error) => return Err(error.into()),
    };

    if response.tool_calls.is_empty() {
        let text = response.text.as_deref().unwrap_or_default().trim();
        return Ok(PermissionReplyResolution::Clarify(
            if text.is_empty() {
                DEFAULT_CLARIFICATION
            } else {
                text
            }
            .to_owned(),
        ));
    }

    let executor = ToolExecutor::new(&tools);
    for tool_call in &response.tool_calls {
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some(&tool_call.id),
            name: &tool_call.name,
            arguments_json: &tool_call.arguments_json,
        })
        .map_err(|_| ApprovalResolverError::MalformedToolCall)?;
        let _ = executor.execute(&call).await;
    }
    let resolved = resolution
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    resolved.ok_or(ApprovalResolverError::MalformedToolCall)
}
