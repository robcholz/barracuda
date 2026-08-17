//! Single-Agent Session orchestration adapter implementation.

use core::task::{Context, Poll};

use barracuda_agent_tool::ToolGroup;

use barracuda_agent::internal::{AgentId, AgentKind};

use super::{AgentNotice, OrchestrationHost, OrchestrationPhysicalError};

#[derive(Default)]
pub(crate) struct SessionOrchestration;

impl SessionOrchestration {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn tool_groups(&self, caller: AgentId, kind: &AgentKind) -> Vec<ToolGroup> {
        let _ = (caller, kind);
        Vec::new()
    }

    pub(crate) fn register_root(&mut self, id: AgentId, kind: AgentKind) -> bool {
        let _ = (id, kind);
        true
    }

    pub(crate) fn observe(&mut self, agent: AgentId, notice: AgentNotice) {
        let _ = agent;
        if let AgentNotice::Completed { text, ok } = notice {
            let _ = (text, ok);
        }
    }

    pub(crate) fn cleanup(&mut self) {}

    pub(crate) fn has_live_children(&self) -> bool {
        false
    }

    pub(crate) fn agent_ids(&self) -> Vec<AgentId> {
        Vec::new()
    }

    pub(crate) fn clear(&mut self) {}

    pub(crate) fn poll(
        &mut self,
        context: &mut Context<'_>,
        host: &mut impl OrchestrationHost,
    ) -> Poll<()> {
        let _ = (context, host);
        Poll::Pending
    }

    pub(crate) fn drain_effects(&mut self, host: &mut impl OrchestrationHost) {
        let _ = host;
    }

    pub(crate) fn finish_reaped(
        &mut self,
        agent: AgentId,
        result: Result<(), OrchestrationPhysicalError>,
    ) -> Option<Result<(), OrchestrationPhysicalError>> {
        let _ = agent;
        Some(result)
    }
}
use alloc::vec::Vec;
