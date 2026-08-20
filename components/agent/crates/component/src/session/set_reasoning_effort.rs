use barracuda_agent_runtime::{ReasoningEffort, SessionId};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

use super::{SessionRegistry, SessionRpcError};

/// Request corresponding to `SessionControl::set_reasoning_effort`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct SetReasoningEffortRequest {
    session: SessionIdDto,
    effort: ReasoningEffortDto,
}

impl SetReasoningEffortRequest {
    /// Creates a reasoning-effort update for `session`.
    #[must_use]
    pub fn new(session: SessionId, effort: ReasoningEffort) -> Self {
        Self {
            session: session.into(),
            effort: effort.into(),
        }
    }

    pub(crate) fn parts(self) -> (SessionId, ReasoningEffort) {
        (self.session.into(), self.effort.into())
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
/// Fixed-layout conversion DTO for Agent `ReasoningEffort`.
pub enum ReasoningEffortDto {
    /// Shortest sound reasoning path.
    Low,
    /// Default balanced reasoning.
    Medium,
    /// Deliberate decomposition and verification.
    High,
    /// Maximum multi-agent reasoning effort.
    Ultra,
}

impl From<ReasoningEffort> for ReasoningEffortDto {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::Ultra => Self::Ultra,
        }
    }
}

impl From<ReasoningEffortDto> for ReasoningEffort {
    fn from(value: ReasoningEffortDto) -> Self {
        match value {
            ReasoningEffortDto::Low => Self::Low,
            ReasoningEffortDto::Medium => Self::Medium,
            ReasoningEffortDto::High => Self::High,
            ReasoningEffortDto::Ultra => Self::Ultra,
        }
    }
}

/// RPC corresponding to `SessionControl::set_reasoning_effort`.
pub struct SetReasoningEffort;

impl RpcMethod for SetReasoningEffort {
    const ADDRESS: &'static str = "session.set_reasoning_effort";
    type Request = SetReasoningEffortRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`SetReasoningEffort`].
pub fn set_reasoning_effort_handler(
    registry: SessionRegistry,
) -> impl RpcHandler<SetReasoningEffort> {
    move |_context, request: RpcFrame<SetReasoningEffortRequest>| {
        let registry = registry.clone();
        async move {
            let (session, effort) = request.view()?.parts();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .set_reasoning_effort(effort)
                .await
                .map_err(Into::into))
        }
    }
}
