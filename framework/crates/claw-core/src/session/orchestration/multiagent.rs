//! Multiagent Session orchestration adapter.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{BTreeMap, BTreeSet};

use claw_interface::{Cancel, ClawTimer};
use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};
use crate::multiagent::{
    DispatchOutcome as DomainDispatchOutcome, InterruptOutcome as DomainInterruptOutcome,
    Multiagent, MultiagentEffect, MultiagentHost, MultiagentPhysicalError, SubagentTimeout,
};
use crate::Message;

use super::{
    AgentNotice, HostDispatchOutcome, HostInterruptOutcome, OrchestrationHost,
    OrchestrationPhysicalError, ReapStatus, RemovalOutcome,
};

pub(in crate::session) struct SessionOrchestration<Timer> {
    domain: Multiagent,
    timeouts: AgentTimeouts,
    reaping: BTreeSet<AgentId>,
    next_source: OrchestrationPollSource,
    marker: PhantomData<fn() -> Timer>,
}

impl<Timer> Default for SessionOrchestration<Timer> {
    fn default() -> Self {
        Self {
            domain: Multiagent::new(),
            timeouts: AgentTimeouts::default(),
            reaping: BTreeSet::new(),
            next_source: OrchestrationPollSource::Effect,
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
        self.domain.tool_group(caller, kind).into_iter().collect()
    }

    pub(in crate::session) fn register_root(&mut self, id: AgentId, kind: AgentKind) -> bool {
        self.domain.register_root(id, kind)
    }

    pub(in crate::session) fn observe(&mut self, agent: AgentId, notice: AgentNotice) {
        match notice {
            AgentNotice::Started => self.domain.on_agent_started(agent),
            AgentNotice::AwaitingApproval => self.domain.on_agent_awaiting_approval(agent),
            AgentNotice::ApprovalResolved => self.domain.on_approval_resolved(agent),
            AgentNotice::Idle => self.domain.on_agent_idle(agent),
            AgentNotice::Completed { text, ok } => {
                self.domain.on_agent_completed(agent, text, ok);
            }
            AgentNotice::Cancelled => self.domain.on_agent_cancelled(agent),
        }
    }

    pub(in crate::session) fn cleanup(&mut self) {
        self.domain.cleanup_subagents();
    }

    pub(in crate::session) fn has_live_children(&self) -> bool {
        !self.domain.root_children().is_empty()
    }

    pub(in crate::session) fn agent_ids(&self) -> Vec<AgentId> {
        self.domain.agent_ids()
    }

    pub(in crate::session) fn clear(&mut self) {
        self.domain.clear();
        self.timeouts.entries.clear();
        self.reaping.clear();
    }

    pub(in crate::session) fn finish_reaped(
        &mut self,
        agent: AgentId,
        result: Result<(), OrchestrationPhysicalError>,
    ) -> Option<Result<(), OrchestrationPhysicalError>> {
        if !self.reaping.remove(&agent) {
            return Some(result);
        }
        self.domain.physical_agent_removed(
            agent,
            result.map_err(|error| MultiagentPhysicalError::new(error.to_string())),
        );
        None
    }
}

impl<Timer> SessionOrchestration<Timer>
where
    Timer: ClawTimer + Default + 'static,
{
    pub(in crate::session) fn poll(
        &mut self,
        context: &mut Context<'_>,
        host: &mut impl OrchestrationHost,
    ) -> Poll<()> {
        for _ in 0..2 {
            let source = self.next_source;
            self.next_source = source.next();
            match source {
                OrchestrationPollSource::Effect => {
                    if let Poll::Ready(effect) = self.domain.poll_effect(context) {
                        if let Some(effect) = effect {
                            self.execute(effect, host);
                        }
                        return Poll::Ready(());
                    }
                }
                OrchestrationPollSource::Timeout => {
                    if let Poll::Ready(Some((agent, _timeout))) =
                        self.timeouts.poll_expired(context)
                    {
                        self.domain.timeout(agent);
                        return Poll::Ready(());
                    }
                }
            }
        }
        Poll::Pending
    }

    pub(in crate::session) fn drain_effects(&mut self, host: &mut impl OrchestrationHost) {
        while let Some(effect) = self.domain.take_effect() {
            self.execute(effect, host);
        }
    }

    fn execute(&mut self, effect: MultiagentEffect, host: &mut impl OrchestrationHost) {
        let mut host = DomainHostAdapter::<Timer, _> {
            host,
            timeouts: &mut self.timeouts,
            reaping: &mut self.reaping,
            marker: PhantomData,
        };
        self.domain.execute_effect(effect, &mut host);
    }
}

type TimeoutFuture = Pin<Box<dyn Future<Output = AgentId>>>;

struct TimeoutEntry {
    timeout: SubagentTimeout,
    future: TimeoutFuture,
}

#[derive(Default)]
struct AgentTimeouts {
    entries: BTreeMap<AgentId, TimeoutEntry>,
}

impl AgentTimeouts {
    fn arm<Timer>(&mut self, agent: AgentId, timeout: SubagentTimeout)
    where
        Timer: ClawTimer + Default + 'static,
    {
        let future = Box::pin(async move {
            let mut timer = Timer::default();
            let _ = timer.sleep(timeout.duration(), Cancel::never()).await;
            agent
        });
        self.entries.insert(agent, TimeoutEntry { timeout, future });
    }

    fn remove(&mut self, agent: AgentId) {
        self.entries.remove(&agent);
    }

    fn poll_expired(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<(AgentId, SubagentTimeout)>> {
        let expired = self.entries.iter_mut().find_map(|(&agent, entry)| {
            entry
                .future
                .as_mut()
                .poll(context)
                .is_ready()
                .then_some((agent, entry.timeout))
        });
        let Some((agent, timeout)) = expired else {
            return Poll::Pending;
        };
        self.entries.remove(&agent);
        Poll::Ready(Some((agent, timeout)))
    }
}

#[derive(Clone, Copy)]
enum OrchestrationPollSource {
    Effect,
    Timeout,
}

impl OrchestrationPollSource {
    fn next(self) -> Self {
        match self {
            Self::Effect => Self::Timeout,
            Self::Timeout => Self::Effect,
        }
    }
}

struct DomainHostAdapter<'a, Timer, Host> {
    host: &'a mut Host,
    timeouts: &'a mut AgentTimeouts,
    reaping: &'a mut BTreeSet<AgentId>,
    marker: PhantomData<fn() -> Timer>,
}

impl<Timer, Host> MultiagentHost for DomainHostAdapter<'_, Timer, Host>
where
    Timer: ClawTimer + Default + 'static,
    Host: OrchestrationHost,
{
    fn allocate_agent_id(&mut self) -> AgentId {
        self.host.allocate_agent_id()
    }

    fn create_agent(
        &mut self,
        agent: AgentId,
        kind: &AgentKind,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(), MultiagentPhysicalError> {
        self.host
            .create_agent(agent, kind, extension_tools)
            .map_err(|error| MultiagentPhysicalError::new(error.to_string()))
    }

    fn rollback_agent(&mut self, agent: AgentId) {
        if self.host.rollback_agent(agent) == ReapStatus::Pending {
            self.reaping.insert(agent);
        }
    }

    fn dispatch_agent(&mut self, target: AgentId, message: Message) -> DomainDispatchOutcome {
        match self.host.dispatch_agent(target, message) {
            HostDispatchOutcome::Accepted => DomainDispatchOutcome::Accepted,
            HostDispatchOutcome::Busy => DomainDispatchOutcome::Busy,
            HostDispatchOutcome::Missing => DomainDispatchOutcome::Missing,
        }
    }

    fn interrupt_agent(&mut self, target: AgentId) -> DomainInterruptOutcome {
        match self.host.interrupt_agent(target) {
            HostInterruptOutcome::Accepted => DomainInterruptOutcome::Accepted,
            HostInterruptOutcome::Inactive => DomainInterruptOutcome::Inactive,
            HostInterruptOutcome::Missing => DomainInterruptOutcome::Missing,
        }
    }

    fn begin_remove_agents(
        &mut self,
        agents: Vec<AgentId>,
    ) -> Vec<(AgentId, Result<(), MultiagentPhysicalError>)> {
        for agent in &agents {
            self.timeouts.remove(*agent);
        }
        self.host
            .begin_remove_agents(agents)
            .into_iter()
            .filter_map(|outcome| match outcome {
                RemovalOutcome::Pending(agent) => {
                    self.reaping.insert(agent);
                    None
                }
                RemovalOutcome::Complete { agent, result } => Some((
                    agent,
                    result.map_err(|error| MultiagentPhysicalError::new(error.to_string())),
                )),
            })
            .collect()
    }

    fn arm_timeout(&mut self, agent: AgentId, timeout: SubagentTimeout) {
        self.timeouts.arm::<Timer>(agent, timeout);
    }
}
