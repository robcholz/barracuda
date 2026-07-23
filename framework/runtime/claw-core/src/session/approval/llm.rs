//! LLM-backed implementation of the Session approval resolver.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use claw_api::{ChatRequest, ClawApiAsync, RetryPolicy, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolRunner, ToolSet, ToolSpec,
};
use futures_lite::StreamExt as _;
use serde_json::{json, Value};

use crate::agent::ApprovalDecision;
use crate::config::{ApiPurpose, SharedApiManager};
use crate::session::Message;

use super::{ApprovalFuture, ApprovalResolver, ApprovalResolverError};

const APPROVAL_RESOLVER_PROMPT: &str = prompt!("approval/resolver_system.md");
const USER_REJECTED: &str = "user rejected";

pub(crate) struct LlmApprovalResolver<Http, Timer> {
    api_manager: SharedApiManager,
    marker: PhantomData<fn() -> (Http, Timer)>,
}

impl<Http, Timer> LlmApprovalResolver<Http, Timer> {
    pub(crate) fn new(api_manager: SharedApiManager) -> Self {
        Self {
            api_manager,
            marker: PhantomData,
        }
    }
}

impl<Http, Timer> ApprovalResolver for LlmApprovalResolver<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    async fn resolve(
        self: Rc<Self>,
        tool_call: ToolCall,
        reason: String,
        reply: Message,
    ) -> Result<ApprovalDecision, ApprovalResolverError> {
        let api_manager = Arc::clone(&self.api_manager);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let future: ApprovalFuture = Box::pin(async move {
            resolve_permission_reply::<Http, Timer>(
                &api_manager,
                &tool_call,
                &reason,
                reply.as_str(),
                task_cancelled.as_ref(),
            )
            .await
        });
        CancellableApprovalFuture { cancelled, future }.await
    }
}

struct CancellableApprovalFuture {
    cancelled: Arc<AtomicBool>,
    future: ApprovalFuture,
}

impl Future for CancellableApprovalFuture {
    type Output = Result<ApprovalDecision, ApprovalResolverError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

impl Drop for CancellableApprovalFuture {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct ResolvePermissionReplyTool {
    resolution: Arc<Mutex<Option<ApprovalDecision>>>,
}

impl ResolvePermissionReplyTool {
    fn new(resolution: Arc<Mutex<Option<ApprovalDecision>>>) -> Self {
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
            "yes" => ApprovalDecision::Approved,
            "no" => ApprovalDecision::Rejected(USER_REJECTED.to_owned()),
            "other" => {
                let Some(reason) = args
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    return Err(ToolError::InvalidArguments(
                        "reason is required when decision is other".into(),
                    )
                    .into());
                };
                ApprovalDecision::Rejected(reason.to_owned())
            }
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "decision must be yes|no|other, got '{other}'"
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

async fn resolve_permission_reply<Http, Timer>(
    api_manager: &SharedApiManager,
    tool_call: &ToolCall,
    reason: &str,
    user_reply: &str,
    cancelled: &AtomicBool,
) -> Result<ApprovalDecision, ApprovalResolverError>
where
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let mut llm = ClawApiAsync::<Http, Timer>::new(Http::default(), Timer::default());
    if let Some(config) = api_manager
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_api(ApiPurpose::RootAgent)
    {
        llm.set_config(config)?;
    }
    let resolution = Arc::new(Mutex::new(None));
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
    let response = llm.chat(&request, Cancel::new(cancelled)).await?;
    let [tool_call] = response.tool_calls.as_slice() else {
        return Err(ApprovalResolverError::MalformedToolCall);
    };

    let runner = ToolRunner::new(&tools);
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
    let resolved = resolution
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    resolved.ok_or(ApprovalResolverError::MalformedToolCall)
}
