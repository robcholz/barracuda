//! Session lifecycle, public stream/control API, and actor-owned state.

mod actor;
mod agent_slot;
mod approval_resolver;
mod control;
mod manager;
mod message;
mod permission;
mod persistence;
mod state;
mod stream;

pub use approval_resolver::ApprovalResolverError;
pub use control::{SessionControl, SessionControlError};
pub use manager::{OpenSessionError, SessionCreateError, SessionId, SessionPersistence};
pub(crate) use manager::{SessionManager, SessionManagerInitError};
pub use message::Message;
pub use stream::{
    InputRequestId, InputRequestKind, IterationEvent, SessionCloseReason, SessionError,
    SessionEvent, SessionEventError, SessionInputError, SessionStream, SessionTurnError, TurnEvent,
    TurnEventError, TurnId, TurnOrigin,
};
