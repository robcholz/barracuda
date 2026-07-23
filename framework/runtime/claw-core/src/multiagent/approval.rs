use claw_api::ToolCall;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};

use crate::agent::{AgentApprovalError, ApprovalDecision, ToolCallId};
use crate::protocol::{AgentId, InputRequestKind, TurnOrigin};

use super::{DriveOutput, MultiagentRuntime};

/// Input the multiagent runtime needs before it can continue.
///
/// This is a boundary value, not a handle to the approval queue. The session
/// owns whether the request has already been presented to its caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MultiagentInputRequest {
    idle_origin: TurnOrigin,
    kind: InputRequestKind,
}

impl MultiagentInputRequest {
    pub(crate) fn into_parts(self) -> (TurnOrigin, InputRequestKind) {
        (self.idle_origin, self.kind)
    }

    pub(crate) fn kind(&self) -> &InputRequestKind {
        &self.kind
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum ApprovalResolutionError {
    #[error("no active approval to resolve")]
    NoActiveApproval,
    #[error("no agent {0} to resolve approval for")]
    UnknownAgent(AgentId),
    #[error(transparent)]
    Input(AgentApprovalError),
}

impl<Filesystem, Http, Timer> MultiagentRuntime<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    pub(crate) fn required_input(&self) -> Option<MultiagentInputRequest> {
        let (agent, approval) = self.state.active_approval()?;
        Some(MultiagentInputRequest {
            idle_origin: TurnOrigin::Subagent { agent },
            kind: InputRequestKind::PermissionApproval {
                tool_call: approval.tool_call.clone(),
                reason: approval.reason.clone(),
            },
        })
    }

    pub(crate) fn resolve_required_input(
        &mut self,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalResolutionError> {
        let (agent, tool_call_id) = self
            .state
            .active_approval()
            .map(|(agent, approval)| (agent, approval.tool_call_id))
            .ok_or(ApprovalResolutionError::NoActiveApproval)?;
        let decision_name: &'static str = (&decision).into();
        self.slots
            .resolve_approval(agent, tool_call_id, decision)
            .ok_or(ApprovalResolutionError::UnknownAgent(agent))?
            .map_err(ApprovalResolutionError::Input)?;

        let removed = self.state.remove_approval(agent);
        debug_assert!(
            removed,
            "the active approval changed without synchronization"
        );
        tracing::info!(
            name: "approval_resolved",
            agent = %agent,
            decision = decision_name,
        );
        Ok(())
    }

    pub(super) fn park_approval(
        &mut self,
        agent: AgentId,
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    ) -> DriveOutput {
        if !self.state.contains(agent) {
            return DriveOutput::default();
        }
        self.state
            .park_approval(agent, tool_call_id, tool_call, reason);

        DriveOutput::default()
    }

    pub(super) fn has_pending_approval(&self) -> bool {
        self.state.has_pending_approval()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use claw_api::ToolCall;
    use claw_interface::{ImmediateTimer, MemFs, RealHttp};
    use claw_permission::AllowAll;
    use claw_persistence::Persistence;
    use claw_tool::ToolRegistry;

    use crate::agent::{
        AgentApprovalError, ApprovalDecision, FsAgentFactory, PersistenceConfig, ToolCallId,
    };
    use crate::config::{catalog as agent_catalog, SharedApiManager};
    use crate::protocol::{AgentId, Message};

    use super::super::{AgentIdAllocator, AgentPlacement, MultiagentRuntime, MultiagentState};
    use super::ApprovalResolutionError;

    type TestInstance = MultiagentRuntime<MemFs, RealHttp, ImmediateTimer>;

    fn instance() -> TestInstance {
        MemFs::new();
        let factory = FsAgentFactory::new(
            Arc::new(ToolRegistry::new()),
            Arc::new(
                Persistence::<MemFs>::new("/approval-test/state").expect("test persistence builds"),
            ),
            "/approval-test/memory".to_owned(),
            Vec::new(),
            SharedApiManager::default(),
        )
        .expect("test factory builds");
        MultiagentRuntime::new(
            Rc::new(factory),
            AgentIdAllocator::new(),
            Arc::new(AllowAll),
            MultiagentState::default(),
        )
    }

    #[test]
    fn unknown_agent_does_not_consume_active_approval() {
        let agent = AgentId(7);
        let mut instance = instance();
        instance.state.park_approval(
            agent,
            ToolCallId::new(0),
            ToolCall::default(),
            "permission".to_owned(),
        );

        assert!(matches!(
            instance.resolve_required_input(ApprovalDecision::Approved),
            Err(ApprovalResolutionError::UnknownAgent(id)) if id == agent
        ));
        assert_eq!(
            instance.state.active_approval().map(|(agent, _)| agent),
            Some(agent)
        );
    }

    #[test]
    fn rejected_agent_command_does_not_consume_active_approval() {
        let agent = AgentId(7);
        let mut instance = instance();
        let kind = agent_catalog::root_kind().clone();
        instance
            .build_agent(
                agent,
                &kind,
                Message::text(""),
                AgentPlacement::FreshRoot(PersistenceConfig::InMemory),
            )
            .expect("idle test agent builds");
        assert!(instance.state.insert_root(agent, kind));
        instance.state.park_approval(
            agent,
            ToolCallId::new(0),
            ToolCall::default(),
            "permission".to_owned(),
        );

        assert!(matches!(
            instance.resolve_required_input(ApprovalDecision::Approved),
            Err(ApprovalResolutionError::Input(
                AgentApprovalError::NotAwaitingApproval
            ))
        ));
        assert_eq!(
            instance.state.active_approval().map(|(agent, _)| agent),
            Some(agent)
        );
    }
}
