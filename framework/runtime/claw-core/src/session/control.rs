use async_channel::Sender;
use claw_permission::PermissionLevel;
use strum::IntoStaticStr;

use crate::agent::ReasoningEffort;

use super::{InputRequestId, Message, SessionId};

/// Failure sending a command through a session control handle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionControlError {
    #[error("session is closed: {0}")]
    SessionClosed(SessionId),
    #[error("session is not waiting for input: {0}")]
    NotAwaitingInput(SessionId),
    #[error("session {session} is waiting for input request {expected}, not {received}")]
    InputRequestMismatch {
        session: SessionId,
        expected: InputRequestId,
        received: InputRequestId,
    },
    #[error("agent runtime is not running")]
    WorkerStopped,
    #[error("failed to update persisted session state")]
    Persistence,
}

#[derive(Clone, Copy, Debug, IntoStaticStr, PartialEq, Eq)]
pub(super) enum ControlOp {
    #[strum(serialize = "interrupt")]
    Interrupt,
    #[strum(serialize = "cancel")]
    Cancel,
}

pub(super) enum SessionCommand {
    Append {
        lease: u64,
        message: Message,
        ack: Sender<Result<(), SessionControlError>>,
    },
    Respond {
        lease: u64,
        request: InputRequestId,
        message: Message,
        ack: Sender<Result<(), SessionControlError>>,
    },
    Control {
        lease: u64,
        op: ControlOp,
        ack: Sender<Result<(), SessionControlError>>,
    },
    SetReasoningEffort {
        lease: u64,
        effort: ReasoningEffort,
        ack: Sender<Result<(), SessionControlError>>,
    },
    SetPermissionLevel {
        lease: u64,
        level: PermissionLevel,
        ack: Sender<Result<(), SessionControlError>>,
    },
    Close {
        lease: u64,
        ack: Sender<Result<(), SessionControlError>>,
    },
}

/// Cloneable write/control half of an open session.
#[derive(Clone)]
pub struct SessionControl {
    lease: u64,
    commands: Sender<SessionCommand>,
}

impl SessionControl {
    pub(super) fn new(lease: u64, commands: Sender<SessionCommand>) -> Self {
        Self { lease, commands }
    }

    /// Append one message to this session's FIFO inbox.
    ///
    /// This resolves when the actor queues the message, not when its turn ends.
    pub async fn append(&self, message: Message) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::Append {
                lease: self.lease,
                message,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }

    /// Respond to one input request inside the current turn.
    ///
    /// This resolves when the actor accepts the response, not when the turn
    /// resumes or ends. The request id prevents a delayed response from being
    /// applied to a newer request.
    pub async fn respond(
        &self,
        request: InputRequestId,
        message: Message,
    ) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::Respond {
                lease: self.lease,
                request,
                message,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }

    /// Apply a new reasoning effort at the next Agent iteration boundary.
    pub async fn set_reasoning_effort(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::SetReasoningEffort {
                lease: self.lease,
                effort,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }

    /// Apply a new permission level to subsequent action authorizations.
    pub async fn set_permission_level(
        &self,
        level: PermissionLevel,
    ) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::SetPermissionLevel {
                lease: self.lease,
                level,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }

    /// Request a stop at the current BaseAgent iteration boundary.
    pub async fn interrupt(&self) -> Result<(), SessionControlError> {
        self.send_control(ControlOp::Interrupt).await
    }

    /// Cooperatively abort the current BaseAgent task immediately.
    pub async fn cancel(&self) -> Result<(), SessionControlError> {
        self.send_control(ControlOp::Cancel).await
    }

    /// Close this event stream. The session id stays live; dirty state is
    /// persisted by the runtime's global persistence boundary.
    pub async fn close(&self) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::Close {
                lease: self.lease,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }

    async fn send_control(&self, op: ControlOp) -> Result<(), SessionControlError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(SessionCommand::Control {
                lease: self.lease,
                op,
                ack,
            })
            .await
            .map_err(|_| SessionControlError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionControlError::WorkerStopped))
    }
}
