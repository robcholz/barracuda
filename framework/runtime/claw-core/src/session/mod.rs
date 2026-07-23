//! Session lifecycle, public stream/control API, and actor-owned state.

mod actor;
mod agent_slot;
mod api;
mod approval_resolver;
mod command;
mod manager;
mod message;
mod permission_policy;
mod persistent;
mod state;
mod stream;

pub use api::{OpenSessionError, SessionControl, SessionControlError, SessionCreateError};
pub use approval_resolver::ApprovalResolverError;
pub use manager::{SessionId, SessionPersistence};
pub(crate) use manager::{SessionManager, SessionManagerInitError};
pub use message::Message;
pub use stream::{
    InputRequestId, InputRequestKind, IterationEvent, SessionCloseReason, SessionError,
    SessionEvent, SessionEventError, SessionInputError, SessionStream, SessionTurnError, TurnEvent,
    TurnEventError, TurnId, TurnOrigin,
};
