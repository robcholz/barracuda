//! Session approval flow and its shared natural-language resolver boundary.
//!
//! This is deliberately not an agent tool. The channel user replies in free
//! text, and the SessionActor runs one short LLM/tool round to classify that text
//! into the internal [`ApprovalDecision`] it feeds back to the parked agent.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::rc::Rc;

use claw_api::{ChatError, InitError, ToolCall};
use claw_tool::ToolSetError;
use tracing::Instrument as _;

use crate::agent::{ApprovalDecision, ToolCallId};

use super::{InputRequestId, InputRequestKind, Message};

mod llm;

pub(super) use llm::LlmApprovalResolver;

pub(super) type ApprovalFuture =
    Pin<Box<dyn Future<Output = Result<ApprovalDecision, ApprovalResolverError>>>>;

pub(super) trait ApprovalResolver {
    async fn resolve(
        self: Rc<Self>,
        tool_call: ToolCall,
        reason: String,
        reply: Message,
    ) -> Result<ApprovalDecision, ApprovalResolverError>;
}

pub(super) type SharedApprovalResolver<Http, Timer> = Rc<LlmApprovalResolver<Http, Timer>>;

struct ApprovalRequest {
    request: InputRequestId,
    tool_call_id: ToolCallId,
    tool_call: ToolCall,
    reason: String,
}

enum ApprovalState {
    Waiting(ApprovalRequest),
    Resolving {
        request: InputRequestId,
        tool_call_id: ToolCallId,
        future: ApprovalFuture,
    },
}

pub(super) struct ApprovalCompletion {
    request: InputRequestId,
    tool_call_id: ToolCallId,
    result: Result<ApprovalDecision, ApprovalResolverError>,
}

impl ApprovalCompletion {
    pub(super) fn into_parts(
        self,
    ) -> (
        InputRequestId,
        ToolCallId,
        Result<ApprovalDecision, ApprovalResolverError>,
    ) {
        (self.request, self.tool_call_id, self.result)
    }
}

pub(super) enum ApprovalRespondError {
    NotWaiting,
    Resolving,
    RequestMismatch { expected: InputRequestId },
}

pub(super) struct ApprovalFlow<Resolver> {
    resolver: Rc<Resolver>,
    next_request: u32,
    state: Option<ApprovalState>,
}

impl<Resolver> ApprovalFlow<Resolver>
where
    Resolver: ApprovalResolver + 'static,
{
    pub(super) fn new(resolver: Rc<Resolver>) -> Self {
        Self {
            resolver,
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

    pub(super) fn respond(
        &mut self,
        received: InputRequestId,
        message: Message,
    ) -> Result<(), ApprovalRespondError> {
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

        let ApprovalRequest {
            request,
            tool_call_id,
            tool_call,
            reason,
        } = request;
        let future: ApprovalFuture = Box::pin(
            Rc::clone(&self.resolver)
                .resolve(tool_call, reason, message)
                .instrument(tracing::info_span!("approval.resolve")),
        );
        self.state = Some(ApprovalState::Resolving {
            request,
            tool_call_id,
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
        let Some(ApprovalState::Resolving {
            request,
            tool_call_id,
            ..
        }) = self.state.take()
        else {
            unreachable!("a ready approval remains in the resolving state")
        };
        Poll::Ready(Some(ApprovalCompletion {
            request,
            tool_call_id,
            result,
        }))
    }

    pub(super) fn cancel(&mut self) {
        self.state = None;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalResolverError {
    #[error("failed to initialize approval resolver LLM: {0}")]
    Init(#[from] InitError),
    #[error(transparent)]
    ToolSet(#[from] ToolSetError),
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("approval resolver returned a malformed tool call")]
    MalformedToolCall,
}
