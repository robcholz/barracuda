//! Session lifecycle, public stream/control API, and actor-owned state.

mod actor;
mod agent_slot;
mod api;
mod approval_resolver;
mod command;
mod event;
mod manager;
mod manager_state;
mod message;
mod permission_policy;
mod persistent_state;

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;

pub use api::{
    OpenSessionError, SessionControl, SessionControlError, SessionCreateError, SessionStream,
};
pub(crate) use command::SessionEndpoint;
pub use event::{InputRequestKind, SessionEvent};
pub(crate) use manager::{SessionManager, SessionManagerInitError, SessionManagerStatus};
pub use manager_state::SessionId;
pub use message::Message;

crate::define_prefixed_id!(InputRequestId, "input-", "input request");
crate::define_prefixed_id!(TurnId, "turn-", "turn");

/// What caused a root-visible turn to start.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOrigin {
    /// A public caller appended a message.
    #[default]
    User,
    /// A detached subagent delivered its result to its parent.
    Subagent {
        /// The subagent whose result caused this turn.
        agent: AgentId,
    },
}

impl TurnOrigin {
    pub(crate) fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }
}

/// Whether a session survives a runtime restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPersistence {
    /// Persist session state and write the root transcript to storage.
    Persistent,
    /// Keep session state and transcript in memory for this process only.
    Ephemeral,
}
