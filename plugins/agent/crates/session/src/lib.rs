#![no_std]
// Without atomic compare-and-swap (ESP32-C3, ESP32-S2) `tracing` compiles to
// nothing, so values only traced look unused there.
#![cfg_attr(not(target_has_atomic = "ptr"), allow(unused))]
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
