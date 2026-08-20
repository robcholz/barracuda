use barracuda_agent_runtime::SessionId;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct SessionIdDto(u32);

impl From<SessionId> for SessionIdDto {
    fn from(session: SessionId) -> Self {
        Self(session.0)
    }
}

impl From<SessionIdDto> for SessionId {
    fn from(session: SessionIdDto) -> Self {
        Self::new(session.0)
    }
}
