use alloc::{string::String, vec::Vec};
use core::{
    cell::{RefCell, RefMut},
    task::{Context, Poll},
};

use barracuda_runtime_utils::local_channel::{self, Receiver, Sender};
use barracuda_runtime_utils::oneshot;
use portable_atomic_util::Arc;

use barracuda_agent::Message;
use barracuda_agent::{AgentId, AgentKind};

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
    #[error(
        "at most {0} subagents may be live at once; wait for one to finish or delete one before spawning another"
    )]
    LimitReached(usize),
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
    pub(crate) accepted: oneshot::Sender<Result<AgentId, MultiagentCommandError>>,
    pub(crate) completion: oneshot::Sender<SubagentResult>,
}

pub(crate) struct DeleteCommand {
    pub(crate) target: AgentId,
    pub(crate) completed: oneshot::Sender<Result<(), MultiagentCommandError>>,
}

pub(crate) struct FollowupCommand {
    pub(crate) target: AgentId,
    pub(crate) message: Message,
    pub(crate) completed: oneshot::Sender<Result<(), MultiagentCommandError>>,
}

/// Short-lock bridge shared by model-facing tools and one Session actor.
///
/// Commands ride an unbounded async-channel (multi-producer tools, single
/// consumer actor), so channel wakeups replace the former hand-written waker.
/// The read-mostly snapshot sits on its own lock so snapshot reads never
/// contend with command delivery.
pub(crate) struct MultiagentBridge {
    command_tx: Sender<MultiagentCommand>,
    // `Receiver` is `!Unpin`, so it is pinned on the heap to be polled as a
    // `Stream` through the shared bridge.
    command_rx: Receiver<MultiagentCommand>,
    snapshot: RefCell<MultiagentSnapshot>,
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
    ) -> Result<(AgentId, oneshot::Receiver<SubagentResult>), MultiagentCommandError> {
        let (accepted, result) = self
            .bridge
            .spawn(self.caller, SubagentSpec::new(kind, name, goal, timeout));
        let id = accepted
            .await
            .map_err(|_| MultiagentCommandError::BridgeClosed)??;
        Ok((id, result))
    }

    pub(crate) async fn delete(&self, target: AgentId) -> Result<(), MultiagentCommandError> {
        self.bridge
            .delete(self.caller, target)
            .await
            .map_err(|_| MultiagentCommandError::BridgeClosed)?
    }

    /// Deletes `target` and its subtree without waiting for the outcome.
    pub(crate) fn request_delete(&self, target: AgentId) {
        drop(self.bridge.delete(self.caller, target));
    }

    pub(crate) async fn followup(
        &self,
        target: AgentId,
        message: Message,
    ) -> Result<(), MultiagentCommandError> {
        self.bridge
            .followup(self.caller, target, message)
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
        let (command_tx, command_rx) = local_channel::channel();
        Self {
            command_tx,
            command_rx,
            snapshot: RefCell::new(MultiagentSnapshot::default()),
        }
    }

    fn snapshot(&self) -> RefMut<'_, MultiagentSnapshot> {
        self.snapshot.borrow_mut()
    }

    fn push(&self, command: MultiagentCommand) {
        // The bridge holds a sender for its whole life, so the unbounded channel
        // is never closed here and the send cannot fail.
        let _ = self.command_tx.send(command);
    }

    pub(crate) fn spawn(
        &self,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> (
        oneshot::Receiver<Result<AgentId, MultiagentCommandError>>,
        oneshot::Receiver<SubagentResult>,
    ) {
        let (accepted, result) = oneshot::channel();
        let (completion, completed) = oneshot::channel();
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
        match self.command_rx.poll_recv(context) {
            Poll::Ready(Some(command)) => Poll::Ready(command),
            // The bridge keeps a live sender, so the stream never ends; treat a
            // spurious close the same as "no command pending".
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }

    pub(crate) fn clear(&self) {
        // Drain queued commands without closing the channel so later spawns keep
        // delivering.
        while self.command_rx.try_recv().is_ok() {}
        *self.snapshot() = MultiagentSnapshot::default();
    }

    pub(crate) fn publish_snapshot(&self, snapshot: MultiagentSnapshot) {
        *self.snapshot() = snapshot;
    }

    fn delete(
        &self,
        requester: AgentId,
        target: AgentId,
    ) -> oneshot::Receiver<Result<(), MultiagentCommandError>> {
        let (completed, result) = oneshot::channel();
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
    ) -> oneshot::Receiver<Result<(), MultiagentCommandError>> {
        let (completed, result) = oneshot::channel();
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
        self.snapshot().descendants_of(requester)
    }

    fn get(&self, requester: AgentId, target: AgentId) -> Option<SubagentSnapshot> {
        self.snapshot().descendant(requester, target)
    }
}
