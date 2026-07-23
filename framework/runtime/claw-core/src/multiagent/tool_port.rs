use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Waker;

use async_channel::{Receiver, Sender};

use crate::agent::{AgentId, AgentKind};
use crate::session::Message;
use claw_api::ToolCall;

use super::model::{
    MultiagentSnapshot, SubagentResult, SubagentSnapshot, SubagentSpec, SubagentTimeout,
};
/// One command emitted by an agent's subagent tools.
pub(crate) struct MultiagentCommand {
    requester: AgentId,
    action: MultiagentAction,
}

impl MultiagentCommand {
    pub(crate) fn new(requester: AgentId, action: MultiagentAction) -> Self {
        Self { requester, action }
    }

    pub(crate) fn into_parts(self) -> (AgentId, MultiagentAction) {
        (self.requester, self.action)
    }
}

pub(crate) enum MultiagentAction {
    Spawn(SpawnCommand),
    Delete { target: AgentId },
    Followup { target: AgentId, message: Message },
}

pub(crate) struct SpawnCommand {
    spec: SubagentSpec,
    accepted: Sender<Result<AgentId, String>>,
    completion: Option<Sender<SubagentResult>>,
    source_call: Option<ToolCall>,
}

impl SpawnCommand {
    pub(crate) fn into_parts(
        self,
    ) -> (
        SubagentSpec,
        Sender<Result<AgentId, String>>,
        Option<Sender<SubagentResult>>,
        Option<ToolCall>,
    ) {
        (self.spec, self.accepted, self.completion, self.source_call)
    }
}

#[derive(Default)]
struct MultiagentBridgeState {
    commands: VecDeque<MultiagentCommand>,
    waiter: Option<Waker>,
    snapshot: MultiagentSnapshot,
}

/// Shared boundary between model-facing subagent tools and one session runtime.
///
/// Tools can only invoke the semantic methods on this bridge. The runtime alone
/// drains commands and publishes the inspection snapshot.
pub(crate) struct MultiagentBridge {
    state: Mutex<MultiagentBridgeState>,
}

/// Caller-bound capability handed to model-facing subagent tools.
///
/// Binding the caller here prevents tools from choosing an arbitrary requester
/// when they enqueue commands or inspect the graph.
pub(crate) struct SubagentControl {
    caller: AgentId,
    bridge: Arc<MultiagentBridge>,
}

impl SubagentControl {
    pub(crate) fn new(caller: AgentId, bridge: Arc<MultiagentBridge>) -> Self {
        Self { caller, bridge }
    }

    pub(crate) async fn spawn_background(
        &self,
        kind: AgentKind,
        name: Option<String>,
        goal: Message,
        timeout: SubagentTimeout,
        source_call: ToolCall,
    ) -> Result<AgentId, String> {
        self.bridge
            .spawn_background(
                self.caller,
                SubagentSpec::new(kind, name, goal, timeout),
                source_call,
            )
            .recv()
            .await
            .map_err(|_| "subagent spawn acknowledgement channel closed".to_owned())?
    }

    pub(crate) async fn spawn_foreground(
        &self,
        kind: AgentKind,
        name: Option<String>,
        goal: Message,
        timeout: SubagentTimeout,
    ) -> Result<(AgentId, Receiver<SubagentResult>), String> {
        let (accepted, result) = self
            .bridge
            .spawn_foreground(self.caller, SubagentSpec::new(kind, name, goal, timeout));
        let id = accepted
            .recv()
            .await
            .map_err(|_| "subagent spawn acknowledgement channel closed".to_owned())??;
        Ok((id, result))
    }

    pub(crate) fn delete(&self, target: AgentId) {
        self.bridge.delete(self.caller, target);
    }

    pub(crate) fn followup(&self, target: AgentId, message: Message) {
        self.bridge.followup(self.caller, target, message);
    }

    pub(crate) fn list(&self) -> Vec<SubagentSnapshot> {
        self.bridge.list(self.caller)
    }

    pub(crate) fn get(&self, target: AgentId) -> Option<SubagentSnapshot> {
        self.bridge.get(self.caller, target)
    }
}

impl MultiagentBridge {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(MultiagentBridgeState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, MultiagentBridgeState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn push(&self, command: MultiagentCommand) {
        let waiter = {
            let mut state = self.state();
            state.commands.push_back(command);
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    fn spawn(
        &self,
        parent: AgentId,
        spec: SubagentSpec,
        completion: Option<Sender<SubagentResult>>,
        source_call: Option<ToolCall>,
    ) -> Receiver<Result<AgentId, String>> {
        let (accepted, result) = async_channel::bounded(1);
        self.push(MultiagentCommand::new(
            parent,
            MultiagentAction::Spawn(SpawnCommand {
                spec,
                accepted,
                completion,
                source_call,
            }),
        ));
        result
    }

    pub(crate) fn drain(&self) -> Vec<MultiagentCommand> {
        self.state().commands.drain(..).collect()
    }

    pub(crate) fn clear(&self) {
        let mut state = self.state();
        state.commands.clear();
        state.waiter = None;
    }

    /// Register the drive waiting for an in-flight run. Returns `true` when a
    /// command is already queued and the caller should apply it immediately.
    pub(crate) fn register_waiter(&self, waiter: &Waker) -> bool {
        let mut state = self.state();
        if !state.commands.is_empty() {
            return true;
        }
        state.waiter = Some(waiter.clone());
        false
    }

    pub(crate) fn publish_snapshot(&self, snapshot: MultiagentSnapshot) {
        let mut state = self.state();
        state.snapshot = snapshot;
    }
}

impl MultiagentBridge {
    pub(crate) fn spawn_background(
        &self,
        parent: AgentId,
        spec: SubagentSpec,
        source_call: ToolCall,
    ) -> Receiver<Result<AgentId, String>> {
        self.spawn(parent, spec, None, Some(source_call))
    }

    pub(crate) fn spawn_foreground(
        &self,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> (Receiver<Result<AgentId, String>>, Receiver<SubagentResult>) {
        let (completion, result) = async_channel::bounded(1);
        let accepted = self.spawn(parent, spec, Some(completion), None);
        (accepted, result)
    }

    pub(crate) fn delete(&self, requester: AgentId, target: AgentId) {
        self.push(MultiagentCommand::new(
            requester,
            MultiagentAction::Delete { target },
        ));
    }

    pub(crate) fn followup(&self, requester: AgentId, target: AgentId, message: Message) {
        self.push(MultiagentCommand::new(
            requester,
            MultiagentAction::Followup { target, message },
        ));
    }

    pub(crate) fn list(&self, requester: AgentId) -> Vec<SubagentSnapshot> {
        self.state().snapshot.descendants_of(requester)
    }

    pub(crate) fn get(&self, requester: AgentId, target: AgentId) -> Option<SubagentSnapshot> {
        self.state().snapshot.descendant(requester, target)
    }
}
