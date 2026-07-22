use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};

use crate::agent::FsAgentCreateError;
use crate::config::{catalog as agent_catalog, ReasoningEffort};
use crate::protocol::{AgentId, Message, SessionPersistence};

use super::{AgentPlacement, MultiagentRuntime};

#[derive(Debug, thiserror::Error)]
pub(crate) enum MultiagentDeliverError {
    #[error("failed to build root agent: {0}")]
    Create(#[from] FsAgentCreateError),
    #[error("failed to deliver to root {root}: {source}")]
    Root {
        root: AgentId,
        #[source]
        source: AgentMessageDeliveryError,
    },
}

#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum AgentMessageDeliveryError {
    #[error("no such agent: {0}")]
    UnknownAgent(AgentId),
}

impl<Filesystem, Http, Timer> MultiagentRuntime<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    /// Deliver a message to this session's root.
    pub(crate) fn deliver(
        &mut self,
        message: Message,
        persistence: SessionPersistence,
    ) -> Result<(), MultiagentDeliverError> {
        match self.state.root() {
            Some(root) => self
                .deliver_message(root, message)
                .map_err(|source| MultiagentDeliverError::Root { root, source }),
            None => {
                let id = self.id_allocator.next();
                let kind = agent_catalog::root_kind().clone();
                self.build_agent(
                    id,
                    &kind,
                    message,
                    AgentPlacement::Root {
                        session: self.session,
                        persistence,
                    },
                    Vec::new(),
                )?;
                let inserted = self.state.insert_root(id, kind);
                debug_assert!(inserted, "root insertion requires an empty graph");
                self.enqueue(id);
                Ok(())
            }
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        self.slots.cancel_all();
    }

    /// Update the session default and every currently live Agent independently.
    pub(crate) fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
        self.slots.broadcast_reasoning_effort(effort);
    }

    fn deliver_message(
        &mut self,
        id: AgentId,
        message: Message,
    ) -> Result<(), AgentMessageDeliveryError> {
        if !self.slots.queue_message(id, message) {
            return Err(AgentMessageDeliveryError::UnknownAgent(id));
        }
        self.enqueue(id);
        Ok(())
    }
}
