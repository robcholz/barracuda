use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use futures_lite::future;

use claw_api::ChatStreamEvent;
use claw_utils::stream::StreamPart;

use crate::agent::{AgentEvent, IterationEvent};
use crate::protocol::{AgentId, EventSink, SessionEvent, ToolCall, TurnOrigin};
use crate::scheduler::AgentRun;

use super::agents::{AgentRunEvent, AgentSlotEvent, ReadyAgent};
use super::drive_control::{DriveControl, DriveStop};
use super::timeouts::ExpiredTimeout;
use super::{MultiagentRuntime, MultiagentWork};

enum RuntimeWake {
    Agents(Vec<AgentRunEvent>),
    Timeouts(Vec<ExpiredTimeout>),
}

struct RoutedWake {
    output: DriveOutput,
    root_tool_calls: Option<Vec<ToolCall>>,
}

pub(crate) enum DriveOutcome {
    Complete(DriveOutput, DriveStop),
    BeforeToolCalls(DriveOutput, Vec<ToolCall>),
}

/// Messages created outside the LLM stream and still awaiting engine emission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DriveOutput {
    messages: Vec<String>,
}

impl DriveOutput {
    fn absorb(&mut self, other: DriveOutput) {
        self.messages.extend(other.messages);
    }

    pub(crate) fn message(message: String) -> Self {
        Self {
            messages: vec![message],
        }
    }

    pub(crate) fn into_messages(self) -> Vec<String> {
        self.messages
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnStopMode {
    PreserveAgents,
    DeleteSpawnedAgents,
}

impl<Filesystem, Http, Timer> MultiagentRuntime<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    pub(in crate::multiagent) fn clear_turn_work(&mut self) {
        self.state.clear_turn_work();
        self.slots.clear_inboxes();
        self.pending_deliveries.clear();
        self.root_deliveries_in_turn.clear();
        self.foreground_results.clear();
        self.multiagent.clear();
    }

    pub(crate) fn commit_root_deliveries(&mut self) -> Vec<ToolCall> {
        let children = std::mem::take(&mut self.root_deliveries_in_turn);
        children
            .into_iter()
            .filter_map(|child| self.root_background_spawns.remove(&child))
            .collect()
    }

    pub(crate) fn work(&self) -> MultiagentWork {
        let root = self.state.root();
        let scheduled = self.state.work(
            self.slots.has_running_root(),
            self.slots.has_running_background(),
        );
        if scheduled == MultiagentWork::Root {
            return MultiagentWork::Root;
        }
        let Some(root) = root else {
            return scheduled;
        };
        if self.slots.has_inbox(root) {
            MultiagentWork::Root
        } else if scheduled == MultiagentWork::Background
            || self.slots.has_inbox_except(root)
            || self.timeouts.has_pending()
        {
            MultiagentWork::Background
        } else {
            MultiagentWork::None
        }
    }

    pub(crate) fn pending_root_origin(&self) -> Option<TurnOrigin> {
        let root = self.state.root()?;
        if self.slots.is_running(root) {
            return None;
        }
        self.slots.first_inbox_origin(root)
    }

    pub(crate) fn activate_pending_root_results(&mut self) -> bool {
        let Some(root) = self.state.root() else {
            return false;
        };
        if !self.slots.activate_inbox(root) {
            return false;
        }
        self.root_deliveries_in_turn
            .extend(self.pending_deliveries.take_for_parent(root));
        self.enqueue(root);
        true
    }

    pub(in crate::multiagent) fn enqueue(&mut self, id: AgentId) {
        self.state.enqueue(id);
    }

    pub(in crate::multiagent) fn has_ready(&self) -> bool {
        self.state.has_ready()
    }

    pub(in crate::multiagent) fn has_root_work(&self) -> bool {
        let Some(root) = self.state.root() else {
            return false;
        };
        self.state.is_ready(root) || self.slots.has_running_root()
    }

    /// Stop every task owned by the active turn. Agents are first recovered from
    /// in-flight futures, then reset to idle through their normal cancel reducer.
    /// The caller chooses whether the process-local graph is preserved or pruned.
    pub(crate) async fn stop_turn_tasks(&mut self, mode: TurnStopMode) {
        let control = DriveControl::new();

        self.cancel_foreground_results();
        self.slots.cancel_all();
        while self.slots.has_running() {
            let _ = self.slots.next_events(&control).await;
        }

        self.clear_turn_work();
        self.cancel_all();
        while self.has_ready() || self.slots.has_running() {
            self.start_ready_agent_tasks(true);
            if !self.slots.has_running() {
                continue;
            }
            let _ = self.slots.next_events(&control).await;
        }
        self.clear_turn_work();
        if mode == TurnStopMode::DeleteSpawnedAgents {
            self.delete_spawned_subagents();
        }
        self.refresh_multiagent_snapshot();
    }

    /// Drive the root-visible foreground turn until the root is no longer ready
    /// or in flight. Background subagents may remain in flight after this
    /// returns; they stay on the instance and continue through
    /// [`drive_background_until_root_ready`](Self::drive_background_until_root_ready).
    pub(crate) async fn drive_root_turn(
        &mut self,
        control: &DriveControl,
        events: &EventSink,
    ) -> DriveOutcome {
        let mut output = DriveOutput::default();
        let mut cancel_requested = false;
        let mut interrupt_requested = false;

        loop {
            if control.take_cancel() {
                cancel_requested = true;
                self.cancel_foreground_results();
            }
            if control.take_interrupt() {
                interrupt_requested = true;
                self.slots.interrupt_all();
            }
            if control.take_wake() && !cancel_requested && !interrupt_requested {
                control.clear_cancel_hook();
                return DriveOutcome::Complete(output, DriveStop::Woken);
            }

            if !cancel_requested {
                self.start_ready_agent_tasks(false);
            }

            if cancel_requested {
                if !self.slots.has_running() {
                    control.clear_cancel_hook();
                    return DriveOutcome::Complete(output, DriveStop::Cancelled);
                }
            } else if interrupt_requested {
                if !self.slots.has_running_root() {
                    control.clear_cancel_hook();
                    return DriveOutcome::Complete(output, DriveStop::Interrupted);
                }
            } else if !self.has_root_work() && !self.has_pending_approval() {
                break;
            }

            if !self.slots.has_running() {
                if self.has_ready() {
                    continue;
                }
                break;
            }

            self.set_cancel_hook(control);

            let wake = self.next_runtime_wake(control).await;
            let routed = self.route_runtime_wake(wake, events);
            output.absorb(routed.output);
            if let Some(calls) = routed.root_tool_calls {
                control.clear_cancel_hook();
                return DriveOutcome::BeforeToolCalls(output, calls);
            }
            if self.has_pending_approval() {
                // A foreground root tool may still be waiting on the child
                // that asked. Keep its future in the slot and return control
                // to the session so the caller can answer.
                break;
            }
        }
        control.clear_cancel_hook();
        DriveOutcome::Complete(output, DriveStop::Quiescent)
    }

    /// Poll background agents until they either make the root ready, run out of
    /// work, or are woken by a newer user command.
    pub(crate) async fn drive_background_until_root_ready(
        &mut self,
        control: &DriveControl,
        events: &EventSink,
    ) -> (DriveOutput, DriveStop) {
        let mut output = DriveOutput::default();
        loop {
            if control.take_cancel() {
                control.clear_cancel_hook();
                return (output, DriveStop::Cancelled);
            }
            if control.take_interrupt() {
                control.clear_cancel_hook();
                return (output, DriveStop::Interrupted);
            }
            if control.take_wake() {
                control.clear_cancel_hook();
                return (output, DriveStop::Woken);
            }
            if self.pending_root_origin().is_some()
                || self.has_root_work()
                || self.has_pending_approval()
            {
                control.clear_cancel_hook();
                return (output, DriveStop::Quiescent);
            }

            self.start_ready_agent_tasks(false);
            if self.pending_root_origin().is_some()
                || self.has_root_work()
                || self.has_pending_approval()
            {
                control.clear_cancel_hook();
                return (output, DriveStop::Quiescent);
            }

            if !self.slots.has_running() && !self.timeouts.has_pending() {
                if self.has_ready() {
                    continue;
                }
                break;
            }

            self.set_cancel_hook(control);

            let wake = self.next_runtime_wake(control).await;
            let routed = self.route_runtime_wake(wake, events);
            debug_assert!(routed.root_tool_calls.is_none());
            output.absorb(routed.output);
            if self.has_pending_approval() {
                control.clear_cancel_hook();
                return (output, DriveStop::Quiescent);
            }
        }
        control.clear_cancel_hook();
        (output, DriveStop::Quiescent)
    }

    fn set_cancel_hook(&self, control: &DriveControl) {
        let controls = self.slots.controls();
        control.set_cancel_hook(move || {
            for agent in &controls {
                agent.cancel();
            }
        });
    }

    async fn next_runtime_wake(&mut self, control: &DriveControl) -> RuntimeWake {
        if !self.timeouts.has_pending() {
            return RuntimeWake::Agents(
                self.slots
                    .next_events_or_command(control, &self.multiagent)
                    .await,
            );
        }

        if !self.slots.has_running() {
            return future::or(
                async { RuntimeWake::Timeouts(self.timeouts.next_expired().await) },
                async {
                    future::poll_fn(|context| {
                        control.set_waker(context.waker().clone());
                        if control.has_signal() {
                            core::task::Poll::Ready(())
                        } else {
                            core::task::Poll::Pending
                        }
                    })
                    .await;
                    RuntimeWake::Agents(Vec::new())
                },
            )
            .await;
        }

        // `or` is left-biased: when a run and its deadline become ready at
        // the same scheduling boundary, committed agent completion wins.
        future::or(
            async {
                RuntimeWake::Agents(
                    self.slots
                        .next_events_or_command(control, &self.multiagent)
                        .await,
                )
            },
            async { RuntimeWake::Timeouts(self.timeouts.next_expired().await) },
        )
        .await
    }

    /// Start every currently-ready agent in its stable slot.
    fn start_ready_agent_tasks(&mut self, include_root_inbox: bool) {
        self.schedule_pending_inboxes(include_root_inbox);
        let ready = self.drain_ready_agents();
        if ready.is_empty() {
            return;
        }

        for ready in ready {
            let id = ready.id;
            let is_root = ready.is_root;
            let meta = self
                .state
                .node(id)
                .expect("a ready agent must remain in the live graph");
            let span = tracing::info_span!(
                "agent",
                trace.task = %id,
                run.agent = %id,
                kind = %meta.kind().as_str(),
                depth = self
                    .state
                    .depth(id)
                    .expect("live graph topology is valid") as u64,
            );
            let run = AgentRun::start(ready.agent, ready.message);
            let control = run.control();
            self.slots.start(id, is_root, control, span, run);
        }
        self.refresh_multiagent_snapshot();
    }

    /// Apply subagent commands, then route events from running Agents.
    fn route_agent_events(
        &mut self,
        agent_events: Vec<AgentRunEvent>,
        output_events: &EventSink,
    ) -> RoutedWake {
        let mut output = DriveOutput::default();
        let mut root_tool_calls = None;
        self.apply_multiagent_commands();
        for AgentRunEvent { id, event } in agent_events {
            if !self.state.contains(id) {
                continue;
            }
            match event {
                AgentSlotEvent::Event(item) => {
                    if self.state.is_root(id) {
                        match &item {
                            Ok(AgentEvent::Iteration(StreamPart::Delta(
                                IterationEvent::BeforeToolCalls(calls),
                            ))) => {
                                debug_assert!(root_tool_calls.is_none());
                                root_tool_calls = Some(calls.clone());
                            }
                            Ok(AgentEvent::Iteration(event)) => {
                                emit_iteration_event(output_events, event);
                            }
                            Ok(AgentEvent::InputRequired(_))
                            | Ok(AgentEvent::Finished(_))
                            | Err(_) => {}
                        }
                    }
                    output.absorb(match item {
                        Ok(event) => self.route_event(id, event),
                        Err(error) => self.route_error(id, error),
                    });
                }
                AgentSlotEvent::Returned => {}
            }
        }
        self.refresh_multiagent_snapshot();
        RoutedWake {
            output,
            root_tool_calls,
        }
    }

    fn route_runtime_wake(&mut self, wake: RuntimeWake, events: &EventSink) -> RoutedWake {
        match wake {
            RuntimeWake::Agents(agent_events) => self.route_agent_events(agent_events, events),
            RuntimeWake::Timeouts(expired) => {
                let output = self.route_expired_timeouts(expired);
                self.refresh_multiagent_snapshot();
                RoutedWake {
                    output,
                    root_tool_calls: None,
                }
            }
        }
    }

    fn drain_ready_agents(&mut self) -> Vec<ReadyAgent<Http, Timer>> {
        let mut ready_agents = Vec::new();
        while let Some(id) = self.pop_ready() {
            if !self.state.contains(id) {
                continue;
            }
            let Some((agent, message)) = self.slots.take_ready(id) else {
                continue;
            };
            ready_agents.push(ReadyAgent {
                id,
                is_root: self.state.is_root(id),
                agent,
                message,
            });
        }
        ready_agents
    }

    fn schedule_pending_inboxes(&mut self, include_root: bool) {
        let root = self.state.root();
        let pending = self.slots.ready_inbox_ids().collect::<Vec<_>>();
        for id in pending {
            if (include_root || Some(id) != root)
                && self.state.contains(id)
                && !self.state.is_awaiting_approval(id)
                && self.slots.activate_inbox(id)
            {
                let _ = self.pending_deliveries.take_for_parent(id);
                self.enqueue(id);
            }
        }
    }

    fn pop_ready(&mut self) -> Option<AgentId> {
        self.state.pop_ready()
    }
}

fn emit_iteration_event(events: &EventSink, progress: &StreamPart<IterationEvent>) {
    let event = match progress {
        StreamPart::Delta(IterationEvent::Started(iteration)) => SessionEvent::IterationStarted {
            iteration: *iteration,
        },
        StreamPart::Delta(IterationEvent::Llm(ChatStreamEvent::Reasoning(part))) => {
            SessionEvent::Reasoning(part.clone())
        }
        StreamPart::Delta(IterationEvent::Llm(ChatStreamEvent::Output(part))) => {
            SessionEvent::Output(part.clone())
        }
        StreamPart::Delta(IterationEvent::Llm(ChatStreamEvent::ToolCalls(part))) => {
            SessionEvent::ToolCalls(part.clone())
        }
        StreamPart::Delta(IterationEvent::BeforeToolCalls(_)) => return,
        StreamPart::End => SessionEvent::IterationEnded,
    };
    events.emit(event);
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use claw_interface::{ImmediateTimer, MemFs, RealHttp};
    use claw_permission::AllowAll;
    use claw_persistence::Persistence;
    use claw_tool::ToolRegistry;
    use futures_lite::future::block_on;

    use super::super::model::{SubagentResult, SubagentTimeout};
    use super::super::{
        AgentIdAllocator, AgentPlacement, DriveControl, MultiagentRuntime, MultiagentState,
        MultiagentWork,
    };
    use super::DriveOutput;
    use crate::agent::{FsAgentFactory, PersistenceConfig};
    use crate::config::{catalog as agent_catalog, SharedApiManager};
    use crate::protocol::{AgentId, AgentKind, EventSink, Message, TurnOrigin};

    type TestRuntime = MultiagentRuntime<MemFs, RealHttp, ImmediateTimer>;

    #[test]
    fn drive_output_owns_only_messages_that_still_need_emission() {
        let mut output = DriveOutput::message("first".to_owned());
        output.absorb(DriveOutput::message("second".to_owned()));

        assert_eq!(
            output.into_messages(),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn idle_root_inbox_has_priority_over_other_background_work() {
        let (mut runtime, root, child) = runtime_with_root_and_child();
        runtime.enqueue(child);
        assert!(runtime
            .slots
            .deliver_child_result(
                root,
                SubagentResult::new(child, "finished".to_owned(), true),
            )
            .is_some());

        assert_eq!(runtime.work(), MultiagentWork::Root);
    }

    #[test]
    fn activating_root_inbox_delivers_one_message_at_a_time() {
        let (mut runtime, root, child) = runtime_with_root_and_child();
        for index in 0..3 {
            assert!(runtime
                .slots
                .deliver_child_result(
                    root,
                    SubagentResult::new(child, format!("result-{index}"), true),
                )
                .is_some());
        }

        assert!(runtime.activate_pending_root_results());
        assert!(runtime.slots.has_inbox(root));
    }

    #[test]
    fn background_drive_polls_timeouts_even_when_every_agent_is_idle() {
        let (mut runtime, root, child) = runtime_with_root_and_child();
        let timeout = SubagentTimeout::from_millis(25).expect("non-zero timeout");
        runtime.timeouts.arm::<ImmediateTimer>(child, timeout);

        let (_, stop) = block_on(
            runtime.drive_background_until_root_ready(&DriveControl::new(), &EventSink::disabled()),
        );

        assert_eq!(stop, super::DriveStop::Quiescent);
        assert!(!runtime.state.contains(child));
        assert_eq!(
            runtime.pending_root_origin(),
            Some(TurnOrigin::Subagent { agent: child })
        );
        assert!(runtime.state.contains(root));
        assert!(!runtime.timeouts.has_pending());
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn runtime_with_root_and_child() -> (TestRuntime, AgentId, AgentId) {
        MemFs::new();
        let factory = FsAgentFactory::new(
            Arc::new(ToolRegistry::new()),
            Arc::new(
                Persistence::<MemFs>::new("/work-priority-test/state")
                    .expect("test persistence builds"),
            ),
            "/work-priority-test/memory".to_owned(),
            Vec::new(),
            SharedApiManager::default(),
        )
        .expect("test factory builds");
        let mut runtime = MultiagentRuntime::new(
            Rc::new(factory),
            AgentIdAllocator::new(),
            Arc::new(AllowAll),
            MultiagentState::default(),
        );
        let root = AgentId::new(1);
        let child = AgentId::new(2);
        let root_kind = agent_catalog::root_kind().clone();
        let child_kind = AgentKind::from_static("worker");
        runtime
            .build_agent(
                root,
                &root_kind,
                Message::text(""),
                AgentPlacement::FreshRoot(PersistenceConfig::InMemory),
            )
            .expect("root builds");
        assert!(runtime.state.insert_root(root, root_kind));
        runtime
            .build_agent(child, &child_kind, Message::text(""), AgentPlacement::Child)
            .expect("child builds");
        assert!(runtime.state.insert_child(
            root,
            child,
            child_kind,
            None,
            SubagentTimeout::from_millis(60_000).expect("non-zero timeout"),
        ));
        (runtime, root, child)
    }
}
