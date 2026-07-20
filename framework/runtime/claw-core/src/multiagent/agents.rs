use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{btree_map::Entry, BTreeMap, VecDeque};

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};

use crate::agent::{AgentAbortHandle, AgentEvent, AgentRun, BaseAgent, TickOutcome};
use crate::protocol::{AgentId, Message, TurnOrigin};

use super::drive_control::DriveControl;
use super::model::{SubagentResult, TranscriptText};
use super::tool_port::MultiagentBridge;

/// Whether a slot is idle or currently running one agent tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentAvailability {
    Available,
    InFlight,
}

pub(super) struct ReadyAgent<Http: ClawHttp, Timer: ClawTimer> {
    pub(super) id: AgentId,
    /// Only the root's iteration events are forwarded to the session stream.
    pub(super) is_root: bool,
    pub(super) agent: BaseAgent<Http, Timer>,
}

pub(super) struct AgentTickEvent {
    pub(super) id: AgentId,
    pub(super) event: AgentEvent,
}

struct RunningAgent<Http: ClawHttp, Timer: ClawTimer> {
    is_root: bool,
    abort: AgentAbortHandle,
    span: tracing::Span,
    run: AgentRun<Http, Timer>,
}

enum AgentExecution<Http: ClawHttp, Timer: ClawTimer> {
    Idle(BaseAgent<Http, Timer>),
    Running(RunningAgent<Http, Timer>),
}

/// Stable storage for one live graph node.
///
/// The slot owns both forms of the same agent: either the idle `BaseAgent`, or
/// the future currently ticking it. Its inbox remains available in both states.
struct AgentSlot<Http: ClawHttp, Timer: ClawTimer> {
    execution: Option<AgentExecution<Http, Timer>>,
    inbox: VecDeque<Message>,
}

impl<Http: ClawHttp, Timer: ClawTimer> AgentSlot<Http, Timer> {
    fn new(agent: BaseAgent<Http, Timer>) -> Self {
        Self {
            execution: Some(AgentExecution::Idle(agent)),
            inbox: VecDeque::new(),
        }
    }

    fn idle_agent(&self) -> Option<&BaseAgent<Http, Timer>> {
        match self.execution.as_ref()? {
            AgentExecution::Idle(agent) => Some(agent),
            AgentExecution::Running(_) => None,
        }
    }

    fn idle_agent_mut(&mut self) -> Option<&mut BaseAgent<Http, Timer>> {
        match self.execution.as_mut()? {
            AgentExecution::Idle(agent) => Some(agent),
            AgentExecution::Running(_) => None,
        }
    }

    fn take_idle(&mut self) -> Option<BaseAgent<Http, Timer>> {
        match self.execution.take()? {
            AgentExecution::Idle(agent) => Some(agent),
            running @ AgentExecution::Running(_) => {
                self.execution = Some(running);
                None
            }
        }
    }

    fn start(
        &mut self,
        id: AgentId,
        is_root: bool,
        abort: AgentAbortHandle,
        span: tracing::Span,
        run: AgentRun<Http, Timer>,
    ) {
        assert!(
            self.execution
                .replace(AgentExecution::Running(RunningAgent {
                    is_root,
                    abort,
                    span,
                    run,
                }))
                .is_none(),
            "agent slot must be checked out before it starts: {id}"
        );
    }

    fn is_running(&self) -> bool {
        matches!(self.execution, Some(AgentExecution::Running(_)))
    }

    fn is_running_root(&self) -> bool {
        matches!(
            self.execution,
            Some(AgentExecution::Running(RunningAgent { is_root: true, .. }))
        )
    }

    fn abort_handle(&self) -> Option<AgentAbortHandle> {
        match self.execution.as_ref()? {
            AgentExecution::Idle(agent) => Some(agent.abort_handle()),
            AgentExecution::Running(running) => Some(running.abort.clone()),
        }
    }

    fn abort_if_running(&self) -> bool {
        let Some(AgentExecution::Running(running)) = &self.execution else {
            return false;
        };
        running.abort.abort();
        true
    }

    fn activate_inbox(&mut self) -> bool {
        let Some(AgentExecution::Idle(agent)) = self.execution.as_mut() else {
            return false;
        };
        let mut activated = false;
        while let Some(message) = self.inbox.pop_front() {
            agent.activate_deferred_message(message);
            activated = true;
        }
        activated
    }

    fn deliver_child_result(&mut self, result: SubagentResult) -> AgentAvailability {
        self.inbox
            .push_back(Message::from_subagent(result.id(), result.text()));
        if self.is_running() {
            AgentAvailability::InFlight
        } else {
            AgentAvailability::Available
        }
    }
}

/// One session's stable slot collection. A live graph node has exactly one slot.
pub(super) struct AgentSlots<Http: ClawHttp, Timer: ClawTimer> {
    slots: BTreeMap<AgentId, AgentSlot<Http, Timer>>,
}

impl<Http: ClawHttp, Timer: ClawTimer> AgentSlots<Http, Timer> {
    pub(super) fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
        }
    }

    pub(super) fn insert(&mut self, id: AgentId, agent: BaseAgent<Http, Timer>) {
        match self.slots.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(AgentSlot::new(agent));
            }
            Entry::Occupied(_) => panic!("agent slot already exists: {id}"),
        }
    }

    pub(super) fn available_agent_mut(
        &mut self,
        id: AgentId,
    ) -> Option<&mut BaseAgent<Http, Timer>> {
        self.slots.get_mut(&id)?.idle_agent_mut()
    }

    pub(super) fn available_agent(&self, id: AgentId) -> Option<&BaseAgent<Http, Timer>> {
        self.slots.get(&id)?.idle_agent()
    }

    pub(super) fn remove(&mut self, id: AgentId) -> bool {
        if let Some(slot) = self.slots.remove(&id) {
            slot.abort_if_running();
            true
        } else {
            false
        }
    }

    pub(super) fn take_idle(&mut self, id: AgentId) -> Option<BaseAgent<Http, Timer>> {
        self.slots.get_mut(&id)?.take_idle()
    }

    pub(super) fn start(
        &mut self,
        id: AgentId,
        is_root: bool,
        abort: AgentAbortHandle,
        span: tracing::Span,
        run: AgentRun<Http, Timer>,
    ) {
        self.slots
            .get_mut(&id)
            .unwrap_or_else(|| panic!("agent slot is missing: {id}"))
            .start(id, is_root, abort, span, run);
    }

    pub(super) fn activate_inbox(&mut self, id: AgentId) -> bool {
        self.slots
            .get_mut(&id)
            .is_some_and(AgentSlot::activate_inbox)
    }

    pub(super) fn deliver_child_result(
        &mut self,
        parent: AgentId,
        result: SubagentResult,
    ) -> Option<AgentAvailability> {
        Some(self.slots.get_mut(&parent)?.deliver_child_result(result))
    }

    pub(super) fn ready_inbox_ids(&self) -> impl Iterator<Item = AgentId> + '_ {
        self.slots.iter().filter_map(|(&id, slot)| {
            (slot.idle_agent().is_some() && !slot.inbox.is_empty()).then_some(id)
        })
    }

    pub(super) fn has_inbox(&self, id: AgentId) -> bool {
        self.slots
            .get(&id)
            .is_some_and(|slot| !slot.inbox.is_empty())
    }

    pub(super) fn has_inbox_except(&self, excluded: AgentId) -> bool {
        self.slots
            .iter()
            .any(|(&id, slot)| id != excluded && !slot.inbox.is_empty())
    }

    pub(super) fn first_inbox_origin(&self, id: AgentId) -> Option<TurnOrigin> {
        self.slots.get(&id)?.inbox.front().map(Message::origin)
    }

    pub(super) fn clear_inboxes(&mut self) {
        for slot in self.slots.values_mut() {
            slot.inbox.clear();
        }
    }

    pub(super) fn is_running(&self, id: AgentId) -> bool {
        self.slots.get(&id).is_some_and(AgentSlot::is_running)
    }

    pub(super) fn has_running(&self) -> bool {
        self.slots.values().any(AgentSlot::is_running)
    }

    pub(super) fn has_running_root(&self) -> bool {
        self.slots.values().any(AgentSlot::is_running_root)
    }

    pub(super) fn has_running_background(&self) -> bool {
        self.slots
            .values()
            .any(|slot| slot.is_running() && !slot.is_running_root())
    }

    pub(super) fn abort_handles(&self) -> Vec<AgentAbortHandle> {
        self.slots
            .values()
            .filter_map(AgentSlot::abort_handle)
            .collect()
    }

    pub(super) fn abort_all(&self) {
        for slot in self.slots.values() {
            slot.abort_if_running();
        }
    }

    /// Cooperatively abort one running agent so a queued graph effect can retask it.
    pub(in crate::multiagent) fn abort_if_running(&self, id: AgentId) -> bool {
        self.slots.get(&id).is_some_and(AgentSlot::abort_if_running)
    }

    pub(super) fn next_events<'a>(
        &'a mut self,
        control: &'a DriveControl,
    ) -> NextAgentEvents<'a, Http, Timer> {
        NextAgentEvents {
            slots: self,
            control,
            multiagent: None,
        }
    }

    pub(super) fn next_events_or_command<'a>(
        &'a mut self,
        control: &'a DriveControl,
        multiagent: &'a MultiagentBridge,
    ) -> NextAgentEvents<'a, Http, Timer> {
        NextAgentEvents {
            slots: self,
            control,
            multiagent: Some(multiagent),
        }
    }
}

pub(super) struct NextAgentEvents<'a, Http: ClawHttp, Timer: ClawTimer> {
    slots: &'a mut AgentSlots<Http, Timer>,
    control: &'a DriveControl,
    multiagent: Option<&'a MultiagentBridge>,
}

impl<Http, Timer> Future for NextAgentEvents<'_, Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    type Output = Vec<AgentTickEvent>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.control.set_waker(context.waker().clone());
        if this.control.has_signal() {
            return Poll::Ready(Vec::new());
        }
        if this
            .multiagent
            .is_some_and(|host| host.register_waiter(context.waker()))
        {
            return Poll::Ready(Vec::new());
        }

        let mut events = Vec::new();
        let mut pending = false;
        for (&id, slot) in &mut this.slots.slots {
            let mut finished_agent = None;
            let polled = match slot.execution.as_mut() {
                Some(AgentExecution::Running(running)) => {
                    let event = {
                        let _entered = running.span.enter();
                        running.run.poll_event(context)
                    };
                    match event {
                        Poll::Ready(event) => {
                            if let AgentEvent::TickFinished(outcome) = &event {
                                log_tick_outcome(id, running.is_root, outcome);
                                finished_agent = running.run.take_finished_agent();
                            }
                            Some(event)
                        }
                        Poll::Pending => {
                            pending = true;
                            None
                        }
                    }
                }
                Some(AgentExecution::Idle(_)) => None,
                None => panic!("agent slot left in a transition state: {id}"),
            };
            if let Some(event) = polled {
                if let Some(agent) = finished_agent {
                    slot.execution = Some(AgentExecution::Idle(agent));
                }
                events.push(AgentTickEvent { id, event });
            }
        }

        if !events.is_empty() {
            Poll::Ready(events)
        } else if this
            .multiagent
            .is_some_and(|host| host.register_waiter(context.waker()))
        {
            Poll::Ready(Vec::new())
        } else if pending {
            Poll::Pending
        } else {
            Poll::Ready(Vec::new())
        }
    }
}

fn log_tick_outcome(id: AgentId, is_root: bool, outcome: &TickOutcome) {
    match outcome {
        TickOutcome::AwaitingApproval { .. } => {
            tracing::info!(name: "awaiting_approval", agent = %id);
        }
        TickOutcome::Cancelled => {
            if is_root {
                tracing::warn!(name: "root_cancelled", "");
            } else {
                tracing::warn!(name: "subagent_cancelled", agent = %id);
            }
        }
        TickOutcome::Failed(_) => {
            tracing::error!(name: "task_failed", "");
        }
        _ => {}
    }
}
