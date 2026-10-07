//! LLM-backed implementation of the Session approval resolver.

use alloc::{borrow::ToOwned, boxed::Box, format, rc::Rc, string::String, string::ToString, vec};
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolOutput,
    ToolRunner, ToolSet, ToolSpec,
};
use barracuda_model_api::{ChatMessage, ChatRequest, ModelApiFactory, RetryPolicy, ToolCall};
use barracuda_runtime_utils::{Cancel, CancellationFlag};
use futures_lite::StreamExt as _;
use http_client::embedded_nal_async::{Dns, TcpConnect};
use portable_atomic_util::Arc;
use serde::Deserialize;
use serde_json::json;

use barracuda_agent::ApprovalDecision;
use barracuda_agent::Message;
use barracuda_agent::{ApiPurpose, SharedApiManager};

use super::{ApprovalFuture, ApprovalResolver, ApprovalResolverError};

const APPROVAL_RESOLVER_PROMPT: &str = prompt!("approval/resolver_system.md");
const USER_REJECTED: &str = "user rejected";

#[derive(Deserialize)]
struct ResolutionArgs {
    decision: ResolutionDecision,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResolutionDecision {
    Yes,
    No,
    Other,
}

pub(crate) struct LlmApprovalResolver<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    api_manager: SharedApiManager,
    llm_factory: ModelApiFactory<Tcp, Resolver>,
}

impl<Tcp, Resolver> LlmApprovalResolver<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(crate) fn new(
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
    ) -> Self {
        Self {
            api_manager,
            llm_factory,
        }
    }
}

impl<Tcp, Resolver> ApprovalResolver for LlmApprovalResolver<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    async fn resolve(
        self: Rc<Self>,
        tool_call: ToolCall,
        reason: String,
        reply: Message,
    ) -> Result<ApprovalDecision, ApprovalResolverError> {
        let api_manager = Arc::clone(&self.api_manager);
        let llm_factory = self.llm_factory.clone();
        let cancelled = Arc::new(CancellationFlag::new());
        let task_cancelled = Arc::clone(&cancelled);
        let future: ApprovalFuture = Box::pin(async move {
            resolve_permission_reply(
                &api_manager,
                &llm_factory,
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
    cancelled: Arc<CancellationFlag>,
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
        self.cancelled.cancel();
    }
}

struct ResolvePermissionReplyTool {
    resolution: Arc<RefCell<Option<ApprovalDecision>>>,
}

impl ResolvePermissionReplyTool {
    fn new(resolution: Arc<RefCell<Option<ApprovalDecision>>>) -> Self {
        Self { resolution }
    }
}

impl ToolSpec for ResolvePermissionReplyTool {
    tool_metadata!("permission_resolve_reply");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new("permission_resolve_reply", RiskClass::Safe)
    }
}

impl ToolHandler for ResolvePermissionReplyTool {
    type Args = ResolutionArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let resolution = match args.decision {
                ResolutionDecision::Yes => ApprovalDecision::Approved,
                ResolutionDecision::No => ApprovalDecision::Rejected(USER_REJECTED.to_owned()),
                ResolutionDecision::Other => {
                    let Some(reason) = args.reason.map(|reason| reason.trim().to_owned()) else {
                        return Err(ToolError::InvalidArguments(
                            "reason is required when decision is other".into(),
                        )
                        .into());
                    };
                    ApprovalDecision::Rejected(reason)
                }
            };

            *self.resolution.borrow_mut() = Some(resolution);
            Ok(ToolOutput {
                content: "approval reply resolved".to_string(),
                ok: true,
            })
        })
    }
}

async fn resolve_permission_reply(
    api_manager: &SharedApiManager,
    llm_factory: &ModelApiFactory<impl TcpConnect + 'static, impl Dns + 'static>,
    tool_call: &ToolCall,
    reason: &str,
    user_reply: &str,
    cancelled: &CancellationFlag,
) -> Result<ApprovalDecision, ApprovalResolverError> {
    let mut llm = llm_factory.create();
    if let Some(config) = api_manager.borrow().get_api(ApiPurpose::RootAgent) {
        llm.set_config(config)?;
    }
    let resolution = Arc::new(RefCell::new(None));
    let mut tools = ToolSet::empty();
    tools.add_group(ToolGroup::new(
        "permission",
        true,
        [Tool::new(ResolvePermissionReplyTool::new(Arc::clone(
            &resolution,
        )))],
    ))?;
    let tools = tools.begin()?;
    let messages = [ChatMessage::new(&json!({
        "role": "user",
        "content": format!(
            "Pending tool call:\nID: {}\nName: {}\nArguments JSON: {}\n\nPermission reason:\n{reason}\n\nUser reply:\n{user_reply}",
            tool_call.id,
            tool_call.name,
            tool_call.arguments_json,
        )
    }))];
    let request = ChatRequest {
        system_prompt: APPROVAL_RESOLVER_PROMPT,
        messages: &messages,
        reminders: &[],
        tools_json: Some(tools.static_schemas()),
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
    let resolved = resolution.borrow().clone();
    resolved.ok_or(ApprovalResolverError::MalformedToolCall)
}
