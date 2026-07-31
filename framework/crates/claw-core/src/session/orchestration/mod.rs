//! Compile-time selection of the Session orchestration adapter.

// The single-Agent adapter intentionally leaves the physical host port dormant.
#![cfg_attr(not(feature = "multiagent"), allow(dead_code))]

use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};
use crate::Message;

#[cfg(feature = "multiagent")]
mod multiagent;
#[cfg(not(feature = "multiagent"))]
mod single_agent;

#[cfg(feature = "multiagent")]
pub(super) use multiagent::SessionOrchestration;
#[cfg(not(feature = "multiagent"))]
pub(super) use single_agent::SessionOrchestration;

pub(super) enum AgentNotice {
    Started,
    AwaitingApproval,
    ApprovalResolved,
    Idle,
    Completed { text: String, ok: bool },
    Cancelled,
}

pub(super) trait OrchestrationHost {
    fn allocate_agent_id(&mut self) -> AgentId;

    fn create_agent(
        &mut self,
        agent: AgentId,
        kind: &AgentKind,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(), OrchestrationPhysicalError>;

    fn rollback_agent(&mut self, agent: AgentId) -> ReapStatus;

    fn dispatch_agent(&mut self, target: AgentId, message: Message) -> HostDispatchOutcome;

    fn interrupt_agent(&mut self, target: AgentId) -> HostInterruptOutcome;

    fn begin_remove_agents(&mut self, agents: Vec<AgentId>) -> Vec<RemovalOutcome>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostDispatchOutcome {
    Accepted,
    Busy,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostInterruptOutcome {
    Accepted,
    Inactive,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReapStatus {
    Pending,
    Complete,
}

pub(super) enum RemovalOutcome {
    Pending(AgentId),
    Complete {
        agent: AgentId,
        result: Result<(), OrchestrationPhysicalError>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub(super) struct OrchestrationPhysicalError {
    detail: String,
}

impl OrchestrationPhysicalError {
    pub(super) fn new(detail: String) -> Self {
        Self { detail }
    }
}
