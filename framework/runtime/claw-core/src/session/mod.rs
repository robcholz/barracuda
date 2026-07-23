//! Session lifecycle, public stream/control API, and actor-owned state.

mod actor;
mod agent_slot;
mod api;
mod approval_resolver;
mod command;
mod event;
mod manager;
mod message;
mod permission_policy;
mod persistent;
mod state;

pub use api::{
    OpenSessionError, SessionControl, SessionControlError, SessionCreateError, SessionStream,
};
pub(crate) use command::SessionEndpoint;
pub use event::{InputRequestId, InputRequestKind, SessionEvent, TurnId, TurnOrigin};
pub use manager::{SessionId, SessionPersistence};
pub(crate) use manager::{SessionManager, SessionManagerInitError};
pub use message::Message;
