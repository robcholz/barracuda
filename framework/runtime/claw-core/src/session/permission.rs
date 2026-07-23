//! Session-scoped permission policy.

use claw_permission::{PermissionDecision, PermissionPolicy, PermissionRequest};
use claw_persistence::DurableState;

use super::state::SessionPersistentState;

/// Live projection of the durable session permission level.
///
/// Agents share this policy so a session command can affect their next action
/// authorization even while one of them is running outside the actor.
pub(super) struct SessionPermission {
    state: DurableState<SessionPersistentState>,
}

impl SessionPermission {
    pub(super) fn new(state: DurableState<SessionPersistentState>) -> Self {
        Self { state }
    }
}

impl PermissionPolicy for SessionPermission {
    fn evaluate(&self, request: &PermissionRequest<'_>) -> PermissionDecision {
        self.state.get().permission_level.evaluate(request)
    }
}
