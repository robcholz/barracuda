use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use async_channel::{Receiver, Sender};

use crate::agent::{AgentId, AgentKind};
use crate::session::Message;

use super::model::{
    MultiagentSnapshot, SubagentResult, SubagentSnapshot, SubagentSpec, SubagentTimeout,
};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MultiagentCommandError {
    #[error("the requesting agent no longer exists")]
    RequesterMissing,
    #[error("the target agent is not in the requester's subtree")]
    TargetNotControlled,
    #[error("the target agent is busy")]
    TargetBusy,
    #[error("subagent kind '{0}' is not permitted for the requesting agent")]
    ForbiddenKind(String),
    #[error("failed to create subagent: {0}")]
    CreateFailed(String),
    #[error("failed to remove subagent storage: {0}")]
    RemoveFailed(String),
    #[error("the multiagent bridge closed before the command completed")]
    BridgeClosed,
}

/// One command emitted by an Agent's caller-bound subagent tools.
pub(crate) struct MultiagentCommand {
    requester: AgentId,
    action: MultiagentAction,
}

impl MultiagentCommand {
    fn new(requester: AgentId, action: MultiagentAction) -> Self {
        Self { requester, action }
    }

    pub(crate) fn into_parts(self) -> (AgentId, MultiagentAction) {
        (self.requester, self.action)
    }
}

pub(crate) enum MultiagentAction {
    Spawn(SpawnCommand),
    Delete(DeleteCommand),
    Followup(FollowupCommand),
    AcknowledgeDelivery(AgentId),
}

pub(crate) struct SpawnCommand {
    pub(crate) spec: SubagentSpec,
    pub(crate) accepted: Sender<Result<AgentId, MultiagentCommandError>>,
    pub(crate) completion: Sender<SubagentResult>,
}

pub(crate) struct DeleteCommand {
    pub(crate) target: AgentId,
    pub(crate) completed: Sender<Result<(), MultiagentCommandError>>,
}

pub(crate) struct FollowupCommand {
    pub(crate) target: AgentId,
    pub(crate) message: Message,
    pub(crate) completed: Sender<Result<(), MultiagentCommandError>>,
}

#[derive(Default)]
struct MultiagentBridgeState {
    commands: VecDeque<MultiagentCommand>,
    waiter: Option<Waker>,
    snapshot: MultiagentSnapshot,
}

/// Short-lock bridge shared by model-facing tools and one Session actor.
pub(crate) struct MultiagentBridge {
    state: Mutex<MultiagentBridgeState>,
}

/// Caller-bound capability handed to model-facing subagent tools.
///
/// The caller id is fixed at construction, so model arguments cannot forge the
/// requester used by graph authorization.
pub(crate) struct SubagentControl {
    caller: AgentId,
    bridge: Arc<MultiagentBridge>,
}

impl SubagentControl {
    pub(crate) fn new(caller: AgentId, bridge: Arc<MultiagentBridge>) -> Self {
        Self { caller, bridge }
    }

    pub(crate) async fn spawn(
        &self,
        kind: AgentKind,
        name: Option<String>,
        goal: Message,
        timeout: SubagentTimeout,
    ) -> Result<(AgentId, Receiver<SubagentResult>), MultiagentCommandError> {
        let (accepted, result) = self
            .bridge
            .spawn(self.caller, SubagentSpec::new(kind, name, goal, timeout));
        let id = accepted
            .recv()
            .await
            .map_err(|_| MultiagentCommandError::BridgeClosed)??;
        Ok((id, result))
    }

    pub(crate) async fn delete(&self, target: AgentId) -> Result<(), MultiagentCommandError> {
        self.bridge
            .delete(self.caller, target)
            .recv()
            .await
            .map_err(|_| MultiagentCommandError::BridgeClosed)?
    }

    pub(crate) async fn followup(
        &self,
        target: AgentId,
        message: Message,
    ) -> Result<(), MultiagentCommandError> {
        self.bridge
            .followup(self.caller, target, message)
            .recv()
            .await
            .map_err(|_| MultiagentCommandError::BridgeClosed)?
    }

    pub(crate) fn list(&self) -> Vec<SubagentSnapshot> {
        self.bridge.list(self.caller)
    }

    pub(crate) fn get(&self, target: AgentId) -> Option<SubagentSnapshot> {
        self.bridge.get(self.caller, target)
    }

    pub(crate) fn acknowledge_delivery(&self, child: AgentId) {
        self.bridge.push(MultiagentCommand::new(
            self.caller,
            MultiagentAction::AcknowledgeDelivery(child),
        ));
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

    pub(crate) fn spawn(
        &self,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> (
        Receiver<Result<AgentId, MultiagentCommandError>>,
        Receiver<SubagentResult>,
    ) {
        let (accepted, result) = async_channel::bounded(1);
        let (completion, completed) = async_channel::bounded(1);
        self.push(MultiagentCommand::new(
            parent,
            MultiagentAction::Spawn(SpawnCommand {
                spec,
                accepted,
                completion,
            }),
        ));
        (result, completed)
    }

    pub(crate) fn poll_command(&self, context: &mut Context<'_>) -> Poll<MultiagentCommand> {
        let mut state = self.state();
        if let Some(command) = state.commands.pop_front() {
            return Poll::Ready(command);
        }
        if state
            .waiter
            .as_ref()
            .is_none_or(|waiter| !waiter.will_wake(context.waker()))
        {
            state.waiter = Some(context.waker().clone());
        }
        Poll::Pending
    }

    pub(crate) fn clear(&self) {
        let mut state = self.state();
        state.commands.clear();
        state.waiter = None;
        state.snapshot = MultiagentSnapshot::default();
    }

    pub(crate) fn publish_snapshot(&self, snapshot: MultiagentSnapshot) {
        self.state().snapshot = snapshot;
    }

    fn delete(
        &self,
        requester: AgentId,
        target: AgentId,
    ) -> Receiver<Result<(), MultiagentCommandError>> {
        let (completed, result) = async_channel::bounded(1);
        self.push(MultiagentCommand::new(
            requester,
            MultiagentAction::Delete(DeleteCommand { target, completed }),
        ));
        result
    }

    fn followup(
        &self,
        requester: AgentId,
        target: AgentId,
        message: Message,
    ) -> Receiver<Result<(), MultiagentCommandError>> {
        let (completed, result) = async_channel::bounded(1);
        self.push(MultiagentCommand::new(
            requester,
            MultiagentAction::Followup(FollowupCommand {
                target,
                message,
                completed,
            }),
        ));
        result
    }

    fn list(&self, requester: AgentId) -> Vec<SubagentSnapshot> {
        self.state().snapshot.descendants_of(requester)
    }

    fn get(&self, requester: AgentId, target: AgentId) -> Option<SubagentSnapshot> {
        self.state().snapshot.descendant(requester, target)
    }
}
