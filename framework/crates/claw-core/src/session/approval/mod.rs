//! Session approval flow and its shared natural-language resolver boundary.
//!
//! This is deliberately not an agent tool. The channel user replies in free
//! text, and the SessionActor runs one short LLM/tool round to classify that text
//! into the internal [`ApprovalDecision`] it feeds back to the parked agent.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

use claw_api::{ChatError, InitError, ToolCall};
use claw_tool::ToolSetError;
use tracing::Instrument as _;

use crate::agent::{AgentId, ApprovalDecision, ToolCallId};

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
    agent: AgentId,
    request: InputRequestId,
    tool_call_id: ToolCallId,
    tool_call: ToolCall,
    reason: String,
}

enum ApprovalState {
    Waiting(ApprovalRequest),
    Resolving {
        agent: AgentId,
        request: InputRequestId,
        tool_call_id: ToolCallId,
        future: ApprovalFuture,
    },
}

pub(super) struct ApprovalCompletion {
    agent: AgentId,
    request: InputRequestId,
    tool_call_id: ToolCallId,
    result: Result<ApprovalDecision, ApprovalResolverError>,
}

impl ApprovalCompletion {
    pub(super) fn into_parts(
        self,
    ) -> (
        AgentId,
        InputRequestId,
        ToolCallId,
        Result<ApprovalDecision, ApprovalResolverError>,
    ) {
        (self.agent, self.request, self.tool_call_id, self.result)
    }
}

pub(super) struct ApprovalDisplay {
    pub(super) request: InputRequestId,
    pub(super) kind: InputRequestKind,
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
    queued: VecDeque<ApprovalRequest>,
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
            queued: VecDeque::new(),
        }
    }

    pub(super) fn request(
        &mut self,
        agent: AgentId,
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    ) -> Option<ApprovalDisplay> {
        let request = InputRequestId(self.next_request);
        self.next_request = self.next_request.saturating_add(1);
        let pending = ApprovalRequest {
            agent,
            request,
            tool_call_id,
            tool_call,
            reason,
        };
        if self.state.is_some() {
            self.queued.push_back(pending);
            None
        } else {
            let display = Self::display(&pending);
            self.state = Some(ApprovalState::Waiting(pending));
            Some(display)
        }
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
            agent,
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
            agent,
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
            agent,
            request,
            tool_call_id,
            ..
        }) = self.state.take()
        else {
            unreachable!("a ready approval remains in the resolving state")
        };
        Poll::Ready(Some(ApprovalCompletion {
            agent,
            request,
            tool_call_id,
            result,
        }))
    }

    pub(super) fn activate_next(&mut self) -> Option<ApprovalDisplay> {
        if self.state.is_some() {
            return None;
        }
        let pending = self.queued.pop_front()?;
        let display = Self::display(&pending);
        self.state = Some(ApprovalState::Waiting(pending));
        Some(display)
    }

    pub(super) fn cancel_agent(&mut self, agent: AgentId) -> Option<ApprovalDisplay> {
        self.cancel_agents(&BTreeSet::from([agent]))
    }

    pub(super) fn cancel_agents(&mut self, agents: &BTreeSet<AgentId>) -> Option<ApprovalDisplay> {
        self.queued
            .retain(|pending| !agents.contains(&pending.agent));
        let cancel_active = self.state.as_ref().is_some_and(|state| match state {
            ApprovalState::Waiting(pending) => agents.contains(&pending.agent),
            ApprovalState::Resolving { agent, .. } => agents.contains(agent),
        });
        if cancel_active {
            self.state = None;
        }
        self.activate_next()
    }

    pub(super) fn cancel(&mut self) {
        self.state = None;
        self.queued.clear();
    }

    fn display(pending: &ApprovalRequest) -> ApprovalDisplay {
        ApprovalDisplay {
            request: pending.request,
            kind: InputRequestKind::PermissionApproval {
                tool_call: pending.tool_call.clone(),
                reason: pending.reason.clone(),
            },
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    struct ImmediateResolver;

    impl ApprovalResolver for ImmediateResolver {
        async fn resolve(
            self: Rc<Self>,
            _tool_call: ToolCall,
            _reason: String,
            _reply: Message,
        ) -> Result<ApprovalDecision, ApprovalResolverError> {
            Ok(ApprovalDecision::Approved)
        }
    }

    #[test]
    fn approvals_are_displayed_fifo_across_agents() {
        let mut flow = ApprovalFlow::new(Rc::new(ImmediateResolver));
        let first = flow
            .request(
                AgentId(1),
                ToolCallId::new(1),
                ToolCall::default(),
                "first".to_owned(),
            )
            .expect("first approval is displayed");
        assert_eq!(first.request, InputRequestId(1));
        assert!(flow
            .request(
                AgentId(2),
                ToolCallId::new(2),
                ToolCall::default(),
                "second".to_owned(),
            )
            .is_none());

        let second = flow
            .cancel_agent(AgentId(1))
            .expect("cancelling the active approval displays the next one");
        assert_eq!(second.request, InputRequestId(2));
    }

    #[test]
    fn subtree_cancellation_removes_only_matching_queued_approvals() {
        let mut flow = ApprovalFlow::new(Rc::new(ImmediateResolver));
        let _ = flow.request(
            AgentId(1),
            ToolCallId::new(1),
            ToolCall::default(),
            "active".to_owned(),
        );
        let _ = flow.request(
            AgentId(2),
            ToolCallId::new(2),
            ToolCall::default(),
            "removed".to_owned(),
        );
        let _ = flow.request(
            AgentId(3),
            ToolCallId::new(3),
            ToolCall::default(),
            "survives".to_owned(),
        );

        assert!(flow.cancel_agent(AgentId(2)).is_none());
        let next = flow
            .cancel_agent(AgentId(1))
            .expect("the unrelated queued approval remains");
        assert_eq!(next.request, InputRequestId(3));
    }
}
