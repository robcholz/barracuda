//! Internal natural-language approval resolver for one session actor.
//!
//! This is deliberately not an agent tool. The channel user replies in free
//! text, and the SessionActor runs one short LLM/tool round to classify that text
//! into the internal [`ApprovalDecision`] it feeds back to the parked agent.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use claw_api::{ChatError, ChatRequest, ClawApiAsync, InitError, RetryPolicy, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolRunner, ToolSet, ToolSetError, ToolSpec,
};
use futures_lite::StreamExt as _;
use serde_json::{json, Value};
use tracing::Instrument as _;

use crate::agent::{ApprovalDecision, ToolCallId};
use crate::config::{ApiUsage, SharedApiManager};

use super::{InputRequestId, InputRequestKind, Message};

const APPROVAL_RESOLVER_PROMPT: &str = prompt!("approval/resolver_system.md");

const DEFAULT_CLARIFICATION: &str = "Please clearly reply with approval or rejection.";
const DEFAULT_REJECTION: &str = "rejected";

type ApprovalFuture =
    Pin<Box<dyn Future<Output = Result<PermissionReplyResolution, ApprovalResolverError>>>>;

struct ApprovalRequest {
    request: InputRequestId,
    tool_call_id: ToolCallId,
    tool_call: ToolCall,
    reason: String,
}

enum ApprovalState {
    Waiting(ApprovalRequest),
    Resolving {
        request: ApprovalRequest,
        control: ApprovalControl,
        future: ApprovalFuture,
    },
}

pub(super) struct ApprovalCompletion {
    request: ApprovalRequest,
    result: Result<PermissionReplyResolution, ApprovalResolverError>,
}

impl ApprovalCompletion {
    pub(super) fn into_parts(
        self,
    ) -> (
        InputRequestId,
        ToolCallId,
        ToolCall,
        String,
        Result<PermissionReplyResolution, ApprovalResolverError>,
    ) {
        (
            self.request.request,
            self.request.tool_call_id,
            self.request.tool_call,
            self.request.reason,
            self.result,
        )
    }
}

pub(super) enum ApprovalRespondError {
    NotWaiting,
    Resolving,
    RequestMismatch { expected: InputRequestId },
}

pub(super) struct ApprovalFlow {
    api_manager: SharedApiManager,
    next_request: u32,
    state: Option<ApprovalState>,
}

impl ApprovalFlow {
    pub(super) fn new(api_manager: SharedApiManager) -> Self {
        Self {
            api_manager,
            next_request: 1,
            state: None,
        }
    }

    pub(super) fn request(
        &mut self,
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    ) -> (InputRequestId, InputRequestKind) {
        let request = InputRequestId(self.next_request);
        self.next_request = self.next_request.saturating_add(1);
        let kind = InputRequestKind::PermissionApproval {
            tool_call: tool_call.clone(),
            reason: reason.clone(),
        };
        self.state = Some(ApprovalState::Waiting(ApprovalRequest {
            request,
            tool_call_id,
            tool_call,
            reason,
        }));
        (request, kind)
    }

    pub(super) fn respond<H, Timer>(
        &mut self,
        received: InputRequestId,
        message: Message,
    ) -> Result<(), ApprovalRespondError>
    where
        H: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    {
        let Some(state) = self.state.take() else {
            return Err(ApprovalRespondError::NotWaiting);
        };
        let request = match state {
            ApprovalState::Waiting(request) => request,
            resolving @ ApprovalState::Resolving { .. } => {
                self.state = Some(resolving);
                return Err(ApprovalRespondError::Resolving);
            }
        };
        if request.request != received {
            let expected = request.request;
            self.state = Some(ApprovalState::Waiting(request));
            return Err(ApprovalRespondError::RequestMismatch { expected });
        }

        let control = ApprovalControl::new();
        let task_control = control.clone();
        let api_manager = Arc::clone(&self.api_manager);
        let tool_call = request.tool_call.clone();
        let reason = request.reason.clone();
        let user_reply = message.as_str().to_owned();
        let future = Box::pin(
            async move {
                resolve_permission_reply::<H, Timer>(
                    &api_manager,
                    &tool_call,
                    &reason,
                    &user_reply,
                    &task_control,
                )
                .await
            }
            .instrument(tracing::info_span!("approval.resolve")),
        );
        self.state = Some(ApprovalState::Resolving {
            request,
            control,
            future,
        });
        Ok(())
    }

    pub(super) fn poll(&mut self, context: &mut Context<'_>) -> Poll<Option<ApprovalCompletion>> {
        let Some(ApprovalState::Resolving { future, .. }) = self.state.as_mut() else {
            return Poll::Pending;
        };
        let Poll::Ready(result) = future.as_mut().poll(context) else {
            return Poll::Pending;
        };
        let Some(ApprovalState::Resolving { request, .. }) = self.state.take() else {
            unreachable!("a ready approval remains in the resolving state")
        };
        Poll::Ready(Some(ApprovalCompletion { request, result }))
    }

    pub(super) fn cancel(&mut self) {
        if let Some(ApprovalState::Resolving { control, .. }) = self.state.take() {
            control.cancel();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PermissionReplyResolution {
    Approved,
    Rejected(String),
    Clarify(String),
}

impl PermissionReplyResolution {
    pub(super) fn into_decision(self) -> Option<ApprovalDecision> {
        match self {
            Self::Approved => Some(ApprovalDecision::Approved),
            Self::Rejected(reason) => Some(ApprovalDecision::Rejected(reason)),
            Self::Clarify(_) => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalResolverError {
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

#[derive(Clone)]
struct ApprovalControl {
    cancelled: Arc<AtomicBool>,
}

impl ApprovalControl {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
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

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new("permission_resolve_reply", RiskClass::Safe)
    }
}

impl SyncToolHandler for ResolvePermissionReplyTool {
    fn invoke(&self, call: &ToolInvocation) -> Result<ToolOutput, ToolInvokeError> {
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
            content: "approval reply resolved".to_string(),
            ok: true,
        })
    }
}

async fn resolve_permission_reply<H, Timer>(
    api_manager: &SharedApiManager,
    tool_call: &ToolCall,
    reason: &str,
    user_reply: &str,
    control: &ApprovalControl,
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
    let mut tools = ToolSet::empty();
    tools.add_group(ToolGroup::new(
        "permission",
        true,
        [Tool::from_sync(ResolvePermissionReplyTool::new(
            Arc::clone(&resolution),
        ))],
    ))?;
    let tools = tools.begin()?;
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
        .chat(&request, Cancel::new(control.cancelled.as_ref()))
        .await;

    let response = match response {
        Ok(response) => response,
        Err(error) if control.is_cancelled() || error.is_aborted() => {
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

    let runner = ToolRunner::new(&tools);
    for tool_call in &response.tool_calls {
        let call = ToolInvocation::try_new(
            Some(&tool_call.id),
            &tool_call.name,
            &tool_call.arguments_json,
        )
        .map_err(|_| ApprovalResolverError::MalformedToolCall)?;
        let (mut join, detached) = runner.run(vec![call]);
        while join.next().await.is_some() {}
        if let Some(mut detached) = detached {
            while detached.next().await.is_some() {}
        }
    }
    let resolved = resolution
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    resolved.ok_or(ApprovalResolverError::MalformedToolCall)
}
