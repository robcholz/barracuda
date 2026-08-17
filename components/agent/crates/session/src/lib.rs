#![no_std]
#![allow(clippy::arc_with_non_send_sync)]

//! Session lifecycle, public stream/control API, and actor-owned state.

extern crate alloc;

macro_rules! prompt {
    ($path:literal $(,)?) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/prompt/",
            $path
        ))
    };
}

mod actor;
mod agent_slot;
mod approval;
mod control;
mod manager;
mod orchestration;
mod permission;
mod persistence;
mod state;
mod stream;

pub use approval::ApprovalResolverError;
pub use control::{SessionControl, SessionControlError};
pub use manager::{
    OpenSessionError, SessionCreateError, SessionDeleteError, SessionId, SessionPersistence,
};
pub use manager::{SessionManager, SessionManagerInitError};
pub use stream::{
    ContextProviderError, InputRequestId, InputRequestKind, IterationEvent, SessionCloseReason,
    SessionError, SessionEvent, SessionEventError, SessionInputError, SessionStream,
    SessionTurnError, TurnEvent, TurnEventError, TurnId, TurnOrigin,
};
