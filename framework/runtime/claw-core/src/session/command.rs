//! Internal command protocol for one live `SessionActor`.

use async_channel::Sender;
use claw_permission::PermissionLevel;
use strum::IntoStaticStr;

use crate::agent::ReasoningEffort;

use super::api::SessionControlError;
use super::{InputRequestId, Message};

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
