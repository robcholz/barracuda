use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::TranscriptStore;
use claw_persistence::{DurableState, InstanceId};

use crate::agent::AgentState;
use crate::protocol::AgentId;

use super::error::FsAgentCreateError;
use super::FsAgentFactory;

const AGENT_STATE_NAME: &str = "agents";

fn agent_instance(id: AgentId) -> InstanceId {
    InstanceId::new(id.to_wire()).expect("an AgentId wire value is a valid instance id")
}

impl<Filesystem, Http, Timer> FsAgentFactory<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    pub(crate) fn list_persisted_agents(&self) -> Result<Vec<AgentId>, FsAgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .list()?
            .into_iter()
            .map(|instance| {
                AgentId::from_wire(instance.as_str()).map_err(|_| {
                    FsAgentCreateError::InvalidPersistedAgentId(instance.as_str().to_owned())
                })
            })
            .collect()
    }

    pub(crate) fn remove(&self, id: AgentId) -> Result<(), FsAgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .remove(&agent_instance(id))?;
        TranscriptStore::<Filesystem>::delete(id.0, &self.transcript_dir)?;
        Ok(())
    }

    pub(super) fn load_persisted_agent(
        &self,
        id: AgentId,
    ) -> Result<AgentState, FsAgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .load(&agent_instance(id))?
            .ok_or(FsAgentCreateError::AgentNotFound(id))
    }

    pub(super) fn register_new_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), FsAgentCreateError> {
        let collection = self
            .persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?;
        let instance = agent_instance(id);
        if collection.load(&instance)?.is_some() {
            return Err(FsAgentCreateError::AgentAlreadyExists(id));
        }
        self.register_agent(id, state)
    }

    pub(super) fn register_restored_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), FsAgentCreateError> {
        self.register_agent(id, state)
    }

    fn register_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), FsAgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .register(&agent_instance(id), state)?;
        Ok(())
    }
}
