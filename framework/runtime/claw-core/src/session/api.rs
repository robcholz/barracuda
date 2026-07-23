use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use claw_permission::PermissionLevel;
use claw_persistence::PersistenceError;
use futures_core::Stream;

use crate::agent::ReasoningEffort;

use super::command::{ControlOp, SessionCommand, SessionEndpoint};
use super::{InputRequestId, Message, SessionEvent, SessionId};

/// Failure opening a session event stream.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenSessionError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("session is already open: {0}")]
    AlreadyOpen(SessionId),
    #[error("agent runtime is not running")]
    WorkerStopped,
}

/// Failure creating a session through the session manager.
#[derive(Debug, thiserror::Error)]
pub enum SessionCreateError {
    #[error("agent runtime is not running")]
    WorkerStopped,
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

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

/// Cloneable write/control half of an open session.
#[derive(Clone)]
pub struct SessionControl {
    lease: u64,
    commands: Sender<SessionCommand>,
}

impl SessionControl {
    pub(crate) fn new(endpoint: SessionEndpoint) -> Self {
        let (lease, commands) = endpoint.into_parts();
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
    pub async fn close_session(&self) -> Result<(), SessionControlError> {
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

/// One long-lived Session event stream with its control surface.
///
/// Dropping the stream asynchronously closes its lease; use [`close`](Self::close)
/// when the caller must wait until the active Agent has returned.
pub struct SessionStream {
    control: SessionControl,
    events: Pin<Box<Receiver<SessionEvent>>>,
}

impl SessionStream {
    pub(crate) fn new(endpoint: SessionEndpoint, events: Receiver<SessionEvent>) -> Self {
        Self {
            control: SessionControl::new(endpoint),
            events: Box::pin(events),
        }
    }

    /// Clone the write/control capability without cloning this stream receiver.
    pub fn control(&self) -> SessionControl {
        self.control.clone()
    }

    pub async fn append(&self, message: Message) -> Result<(), SessionControlError> {
        self.control.append(message).await
    }

    pub async fn respond(
        &self,
        request: InputRequestId,
        message: Message,
    ) -> Result<(), SessionControlError> {
        self.control.respond(request, message).await
    }

    pub async fn set_reasoning_effort(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), SessionControlError> {
        self.control.set_reasoning_effort(effort).await
    }

    pub async fn set_permission_level(
        &self,
        level: PermissionLevel,
    ) -> Result<(), SessionControlError> {
        self.control.set_permission_level(level).await
    }

    pub async fn interrupt(&self) -> Result<(), SessionControlError> {
        self.control.interrupt().await
    }

    pub async fn cancel(&self) -> Result<(), SessionControlError> {
        self.control.cancel().await
    }

    pub async fn close(&self) -> Result<(), SessionControlError> {
        self.control.close_session().await
    }
}

impl Stream for SessionStream {
    type Item = SessionEvent;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().events.as_mut().poll_next(context)
    }
}

impl Drop for SessionStream {
    fn drop(&mut self) {
        let (ack, _result) = async_channel::bounded(1);
        let _ = self.control.commands.try_send(SessionCommand::Close {
            lease: self.control.lease,
            ack,
        });
    }
}
