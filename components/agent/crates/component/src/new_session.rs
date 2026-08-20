use alloc::rc::Rc;

use barracuda_agent_runtime::{
    AgentRuntime, RuntimeError, SessionCreateError, SessionId, SessionPersistence,
};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

/// Request corresponding to `AgentRuntime::new_session`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct NewSessionRequest {
    persistence: SessionPersistenceDto,
}

impl NewSessionRequest {
    /// Creates a request from the Agent domain type.
    #[must_use]
    pub fn new(persistence: SessionPersistence) -> Self {
        Self {
            persistence: persistence.into(),
        }
    }

    pub(crate) fn persistence(self) -> SessionPersistence {
        self.persistence.into()
    }
}

/// Response corresponding to `AgentRuntime::new_session`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct NewSessionResponse {
    session: SessionIdDto,
}

impl From<SessionId> for NewSessionResponse {
    fn from(session: SessionId) -> Self {
        Self {
            session: session.into(),
        }
    }
}

impl NewSessionResponse {
    /// Returns the Agent domain Session id.
    #[must_use]
    pub fn session(self) -> SessionId {
        self.session.into()
    }
}

/// Component DTO for [`SessionPersistence`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum SessionPersistenceDto {
    /// Preserve the session across runtime restarts.
    Persistent,
    /// Keep the session only for the current process.
    Ephemeral,
}

impl From<SessionPersistence> for SessionPersistenceDto {
    fn from(value: SessionPersistence) -> Self {
        match value {
            SessionPersistence::Persistent => Self::Persistent,
            SessionPersistence::Ephemeral => Self::Ephemeral,
        }
    }
}

impl From<SessionPersistenceDto> for SessionPersistence {
    fn from(value: SessionPersistenceDto) -> Self {
        match value {
            SessionPersistenceDto::Persistent => Self::Persistent,
            SessionPersistenceDto::Ephemeral => Self::Ephemeral,
        }
    }
}

/// Failure returned by `agent.new_session`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum NewSessionError {
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// Persistent session state could not be initialized.
    Persistence,
}

/// RPC corresponding to `AgentRuntime::new_session`.
pub struct NewSession;

impl RpcMethod for NewSession {
    const ADDRESS: &'static str = "agent.new_session";
    type Request = NewSessionRequest;
    type Response = NewSessionResponse;
    type Error = NewSessionError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`NewSession`].
pub fn new_session_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<NewSession>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, request: RpcFrame<NewSessionRequest>| {
        let runtime = Rc::clone(&runtime);
        async move {
            let persistence = request.view()?.persistence();
            let response = match runtime.new_session(persistence).await {
                Ok(session) => Ok(NewSessionResponse::from(session)),
                Err(RuntimeError::SessionCreate(SessionCreateError::WorkerStopped)) => {
                    Err(NewSessionError::WorkerStopped)
                }
                Err(_error) => Err(NewSessionError::Persistence),
            };
            Ok(response)
        }
    }
}
