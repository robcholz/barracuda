//! Single-Agent Session orchestration adapter implementation.

use core::marker::PhantomData;
use core::task::{Context, Poll};

use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};

use super::{AgentNotice, OrchestrationHost, OrchestrationPhysicalError};

pub(in crate::session) struct SessionOrchestration<Timer> {
    marker: PhantomData<fn() -> Timer>,
}

impl<Timer> Default for SessionOrchestration<Timer> {
    fn default() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

impl<Timer> SessionOrchestration<Timer> {
    pub(in crate::session) fn tool_groups(
        &self,
        caller: AgentId,
        kind: &AgentKind,
    ) -> Vec<ToolGroup> {
        let _ = (caller, kind);
        Vec::new()
    }

    pub(in crate::session) fn register_root(&mut self, id: AgentId, kind: AgentKind) -> bool {
        let _ = (id, kind);
        true
    }

    pub(in crate::session) fn observe(&mut self, agent: AgentId, notice: AgentNotice) {
        let _ = agent;
        if let AgentNotice::Completed { text, ok } = notice {
            let _ = (text, ok);
        }
    }

    pub(in crate::session) fn cleanup(&mut self) {}

    pub(in crate::session) fn has_live_children(&self) -> bool {
        false
    }

    pub(in crate::session) fn agent_ids(&self) -> Vec<AgentId> {
        Vec::new()
    }

    pub(in crate::session) fn clear(&mut self) {}

    pub(in crate::session) fn poll(
        &mut self,
        context: &mut Context<'_>,
        host: &mut impl OrchestrationHost,
    ) -> Poll<()> {
        let _ = (context, host);
        Poll::Pending
    }

    pub(in crate::session) fn drain_effects(&mut self, host: &mut impl OrchestrationHost) {
        let _ = host;
    }

    pub(in crate::session) fn finish_reaped(
        &mut self,
        agent: AgentId,
        result: Result<(), OrchestrationPhysicalError>,
    ) -> Option<Result<(), OrchestrationPhysicalError>> {
        let _ = agent;
        Some(result)
    }
}
