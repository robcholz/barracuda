//! One-session API, actor, durable settings/recovery state, and registry.

mod actor;
mod api;
mod approval;
mod permission;
mod persistence;
mod registry;
mod state;

pub(crate) use actor::{SessionActor, SessionActorExit};
pub use api::{
    OpenSessionError, SessionControl, SessionControlError, SessionCreateError, SessionEventStream,
};
pub(crate) use api::{SessionCommand, SessionEndpoint};
pub(crate) use persistence::{session_instance, SessionState, SESSION_STATE_NAME};
pub(crate) use registry::SessionStore;
