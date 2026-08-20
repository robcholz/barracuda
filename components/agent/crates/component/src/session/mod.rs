//! RPCs corresponding to methods on `SessionControl`.

use alloc::{collections::BTreeMap, rc::Rc};
use core::cell::RefCell;

use barracuda_agent_runtime::{SessionControl, SessionControlError, SessionId};

/// `SessionControl::append` RPC.
pub mod append;
/// `SessionControl::cancel` RPC.
pub mod cancel;
/// `SessionControl::close` RPC.
pub mod close;
/// `SessionControl::interrupt` RPC.
pub mod interrupt;
/// `SessionControl::respond` RPC.
pub mod respond;
/// `SessionControl::set_permission_level` RPC.
pub mod set_permission_level;
/// `SessionControl::set_reasoning_effort` RPC.
pub mod set_reasoning_effort;

use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Shared registry of controls established by `agent.open_session`.
#[derive(Clone, Default)]
pub struct SessionRegistry(Rc<RefCell<BTreeMap<SessionId, SessionControl>>>);

impl SessionRegistry {
    /// Returns a clone of the control for `session`.
    #[must_use]
    pub fn get(&self, session: SessionId) -> Option<SessionControl> {
        self.0.borrow().get(&session).cloned()
    }

    /// Associates an opened control with its session.
    pub fn insert(&self, session: SessionId, control: SessionControl) {
        self.0.borrow_mut().insert(session, control);
    }

    /// Removes and returns a session control.
    pub fn remove(&self, session: SessionId) -> Option<SessionControl> {
        self.0.borrow_mut().remove(&session)
    }

    /// Removes every cached session control.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// Error shared by RPCs backed by an open `SessionControl`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum SessionRpcError {
    /// `agent.open_session` has not established a control handle.
    SessionNotOpen,
    /// The session lease is closed.
    SessionClosed,
    /// The active turn is not awaiting caller input.
    NotAwaitingInput,
    /// The response targets a different input request.
    InputRequestMismatch,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// A variable-length request was malformed.
    InvalidRequest,
}

impl From<SessionControlError> for SessionRpcError {
    fn from(error: SessionControlError) -> Self {
        match error {
            SessionControlError::SessionClosed(_) => Self::SessionClosed,
            SessionControlError::NotAwaitingInput(_) => Self::NotAwaitingInput,
            SessionControlError::InputRequestMismatch { .. } => Self::InputRequestMismatch,
            SessionControlError::WorkerStopped => Self::WorkerStopped,
        }
    }
}
