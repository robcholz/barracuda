use barracuda_agent_runtime::{PermissionLevel, SessionId};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

use super::{SessionRegistry, SessionRpcError};

/// Request corresponding to `SessionControl::set_permission_level`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct SetPermissionLevelRequest {
    session: SessionIdDto,
    level: PermissionLevelDto,
}

impl SetPermissionLevelRequest {
    /// Creates a permission-level update for `session`.
    #[must_use]
    pub fn new(session: SessionId, level: PermissionLevel) -> Self {
        Self {
            session: session.into(),
            level: level.into(),
        }
    }

    pub(crate) fn parts(self) -> (SessionId, PermissionLevel) {
        (self.session.into(), self.level.into())
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
/// Fixed-layout conversion DTO for Agent `PermissionLevel`.
pub enum PermissionLevelDto {
    /// Deny actions with side effects.
    Deny,
    /// Ask before actions with side effects.
    Ask,
    /// Allow every action.
    AllowAll,
}

impl From<PermissionLevel> for PermissionLevelDto {
    fn from(value: PermissionLevel) -> Self {
        match value {
            PermissionLevel::Deny => Self::Deny,
            PermissionLevel::Ask => Self::Ask,
            PermissionLevel::AllowAll => Self::AllowAll,
        }
    }
}

impl From<PermissionLevelDto> for PermissionLevel {
    fn from(value: PermissionLevelDto) -> Self {
        match value {
            PermissionLevelDto::Deny => Self::Deny,
            PermissionLevelDto::Ask => Self::Ask,
            PermissionLevelDto::AllowAll => Self::AllowAll,
        }
    }
}

/// RPC corresponding to `SessionControl::set_permission_level`.
pub struct SetPermissionLevel;

impl RpcMethod for SetPermissionLevel {
    const ADDRESS: &'static str = "session.set_permission_level";
    type Request = SetPermissionLevelRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`SetPermissionLevel`].
pub fn set_permission_level_handler(
    registry: SessionRegistry,
) -> impl RpcHandler<SetPermissionLevel> {
    move |_context, request: RpcFrame<SetPermissionLevelRequest>| {
        let registry = registry.clone();
        async move {
            let (session, level) = request.view()?.parts();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .set_permission_level(level)
                .await
                .map_err(Into::into))
        }
    }
}
