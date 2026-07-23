use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{btree_map::Entry, BTreeMap, VecDeque};

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};

use crate::agent::{
    AgentApprovalError, AgentError, AgentEvent, ApprovalDecision, BaseAgent, ReasoningEffortHandle,
    ToolCallId,
};
use crate::protocol::{AgentId, Message, TurnOrigin};
use crate::scheduler::{AgentRun, AgentRunControl, AgentRunItem};

use super::drive_control::DriveControl;
use super::model::{SubagentResult, TranscriptText};
use super::tool_port::MultiagentBridge;

/// Whether a slot is resident or currently running.
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
    pub(super) message: Message,
}

pub(super) struct AgentRunEvent {
    pub(super) id: AgentId,
    pub(super) event: AgentSlotEvent,
}

pub(super) enum AgentSlotEvent {
    Event(Result<AgentEvent, AgentError>),
    Returned,
}

struct RunningAgent<Http: ClawHttp, Timer: ClawTimer> {
    is_root: bool,
    control: AgentRunControl,
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
/// the active AgentRun. Its inbox remains available in both states.
struct AgentSlot<Http: ClawHttp, Timer: ClawTimer> {
    execution: Option<AgentExecution<Http, Timer>>,
    inbox: VecDeque<Message>,
    reasoning_effort: ReasoningEffortHandle,
}

impl<Http: ClawHttp, Timer: ClawTimer> AgentSlot<Http, Timer> {
    fn new(agent: BaseAgent<Http, Timer>, reasoning_effort: ReasoningEffortHandle) -> Self {
        Self {
            execution: Some(AgentExecution::Idle(agent)),
            inbox: VecDeque::new(),
            reasoning_effort,
        }
    }

    fn idle_agent(&self) -> Option<&BaseAgent<Http, Timer>> {
        match self.execution.as_ref()? {
            AgentExecution::Idle(agent) => Some(agent),
            AgentExecution::Running(_) => None,
        }
    }

    fn take_ready(&mut self) -> Option<(BaseAgent<Http, Timer>, Message)> {
        if self.inbox.is_empty() {
            return None;
        }
        match self.execution.take()? {
            AgentExecution::Idle(agent) => {
                let message = self
                    .inbox
                    .pop_front()
                    .expect("a checked ready inbox has a front message");
                Some((agent, message))
            }
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
        control: AgentRunControl,
        span: tracing::Span,
        run: AgentRun<Http, Timer>,
    ) {
        assert!(
            self.execution
                .replace(AgentExecution::Running(RunningAgent {
                    is_root,
                    control,
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

    fn control(&self) -> Option<AgentRunControl> {
        match self.execution.as_ref()? {
            AgentExecution::Idle(_) => None,
            AgentExecution::Running(running) => Some(running.control.clone()),
        }
    }

    fn cancel_if_running(&self) -> bool {
        let Some(AgentExecution::Running(running)) = &self.execution else {
            return false;
        };
        running.control.cancel();
        true
    }

    fn activate_inbox(&mut self) -> bool {
        let Some(AgentExecution::Idle(agent)) = self.execution.as_ref() else {
            return false;
        };
        agent.is_stopped() && !self.inbox.is_empty()
    }

    fn queue_message(&mut self, message: Message) {
        self.inbox.push_back(message);
    }

    fn resolve_approval(
        &self,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentApprovalError> {
        let Some(AgentExecution::Running(running)) = &self.execution else {
            return Err(AgentApprovalError::NotAwaitingApproval);
        };
        running.control.resolve_approval(tool_call_id, decision)
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

    pub(super) fn insert(
        &mut self,
        id: AgentId,
        agent: BaseAgent<Http, Timer>,
        reasoning_effort: ReasoningEffortHandle,
    ) {
        match self.slots.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(AgentSlot::new(agent, reasoning_effort));
            }
            Entry::Occupied(_) => panic!("agent slot already exists: {id}"),
        }
    }

    pub(super) fn remove(&mut self, id: AgentId) -> bool {
        if let Some(slot) = self.slots.remove(&id) {
            slot.cancel_if_running();
            true
        } else {
            false
        }
    }

    pub(super) fn take_ready(&mut self, id: AgentId) -> Option<(BaseAgent<Http, Timer>, Message)> {
        self.slots.get_mut(&id)?.take_ready()
    }

    pub(super) fn start(
        &mut self,
        id: AgentId,
        is_root: bool,
        control: AgentRunControl,
        span: tracing::Span,
        run: AgentRun<Http, Timer>,
    ) {
        self.slots
            .get_mut(&id)
            .unwrap_or_else(|| panic!("agent slot is missing: {id}"))
            .start(id, is_root, control, span, run);
    }

    pub(super) fn activate_inbox(&mut self, id: AgentId) -> bool {
        self.slots
            .get_mut(&id)
            .is_some_and(AgentSlot::activate_inbox)
    }

    pub(super) fn queue_message(&mut self, id: AgentId, message: Message) -> bool {
        let Some(slot) = self.slots.get_mut(&id) else {
            return false;
        };
        slot.queue_message(message);
        true
    }

    pub(super) fn resolve_approval(
        &self,
        id: AgentId,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Option<Result<(), AgentApprovalError>> {
        Some(
            self.slots
                .get(&id)?
                .resolve_approval(tool_call_id, decision),
        )
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

    pub(super) fn controls(&self) -> Vec<AgentRunControl> {
        self.slots.values().filter_map(AgentSlot::control).collect()
    }

    pub(super) fn cancel_all(&self) {
        for slot in self.slots.values() {
            slot.cancel_if_running();
        }
    }

    pub(super) fn interrupt_all(&self) {
        for control in self.slots.values().filter_map(AgentSlot::control) {
            control.interrupt();
        }
    }

    pub(super) fn broadcast_reasoning_effort(&self, effort: crate::config::ReasoningEffort) {
        for slot in self.slots.values() {
            slot.reasoning_effort.set(effort);
        }
    }

    /// Cooperatively cancel one running agent so a queued graph effect can retask it.
    pub(in crate::multiagent) fn cancel_if_running(&self, id: AgentId) -> bool {
        self.slots
            .get(&id)
            .is_some_and(AgentSlot::cancel_if_running)
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
    type Output = Vec<AgentRunEvent>;

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
            let polled = match slot.execution.as_mut() {
                Some(AgentExecution::Running(running)) => {
                    let event = {
                        let _entered = running.span.enter();
                        running.run.poll_event(context)
                    };
                    match event {
                        Poll::Ready(AgentRunItem::Event(event)) => {
                            Some((AgentSlotEvent::Event(event), None))
                        }
                        Poll::Ready(AgentRunItem::Returned) => {
                            let agent = running
                                .run
                                .take_completed_agent()
                                .expect("completed AgentRun returns its BaseAgent once");
                            Some((AgentSlotEvent::Returned, Some(agent)))
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
            if let Some((event, finished_agent)) = polled {
                if let Some(agent) = finished_agent {
                    slot.execution = Some(AgentExecution::Idle(agent));
                }
                events.push(AgentRunEvent { id, event });
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
