//! Internal command protocol for one live `SessionActor`.

use async_channel::Sender;
use claw_permission::PermissionLevel;
use strum::IntoStaticStr;

use crate::config::ReasoningEffort;

use super::api::{OpenSessionError, SessionControlError};
use super::{InputRequestId, Message, SessionEvent};

#[derive(Clone, Copy, Debug, IntoStaticStr, PartialEq, Eq)]
pub(super) enum ControlOp {
    #[strum(serialize = "interrupt")]
    Interrupt,
    #[strum(serialize = "cancel")]
    Cancel,
}

pub(crate) struct SessionEndpoint {
    lease: u64,
    commands: Sender<SessionCommand>,
}

impl SessionEndpoint {
    pub(super) fn new(lease: u64, commands: Sender<SessionCommand>) -> Self {
        Self { lease, commands }
    }

    pub(super) fn into_parts(self) -> (u64, Sender<SessionCommand>) {
        (self.lease, self.commands)
    }
}

pub(super) enum SessionCommand {
    Open {
        events: Sender<SessionEvent>,
        commands: Sender<SessionCommand>,
        ack: Sender<Result<SessionEndpoint, OpenSessionError>>,
    },
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
    Delete {
        ack: Sender<Result<(), SessionControlError>>,
    },
    Shutdown,
}
