use std::collections::BTreeSet;

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::TranscriptStore;
use claw_persistence::{DurableState, InstanceId};

use super::AgentId;
use crate::agent::state::AgentState;

use super::error::AgentCreateError;
use super::AgentManager;

const AGENT_STATE_NAME: &str = "agents";

fn agent_instance(id: AgentId) -> InstanceId {
    InstanceId::new(id.to_wire()).expect("an AgentId wire value is a valid instance id")
}

impl<Filesystem, Http, Timer> AgentManager<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    /// Delete transcript files whose owning Agent record no longer exists.
    pub(super) fn purge_dead(&self) -> Result<(), AgentCreateError> {
        let agents = self
            .list_persisted_agents()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        for transcript in TranscriptStore::<Filesystem>::list_persisted_ids(&self.transcript_dir)? {
            let agent = AgentId::new(transcript);
            if !agents.contains(&agent) {
                TranscriptStore::<Filesystem>::delete(transcript, &self.transcript_dir)?;
            }
        }
        Ok(())
    }

    pub(crate) fn list_persisted_agents(&self) -> Result<Vec<AgentId>, AgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .list()?
            .into_iter()
            .map(|instance| {
                AgentId::from_wire(instance.as_str()).map_err(|_| {
                    AgentCreateError::InvalidPersistedAgentId(instance.as_str().to_owned())
                })
            })
            .collect()
    }

    pub(crate) fn remove(&self, id: AgentId) -> Result<(), AgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .remove(&agent_instance(id))?;
        TranscriptStore::<Filesystem>::delete(id.0, &self.transcript_dir)?;
        Ok(())
    }

    pub(super) fn load_persisted_agent(&self, id: AgentId) -> Result<AgentState, AgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .load(&agent_instance(id))?
            .ok_or(AgentCreateError::AgentNotFound(id))
    }

    pub(super) fn register_new_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), AgentCreateError> {
        let collection = self
            .persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?;
        let instance = agent_instance(id);
        if collection.load(&instance)?.is_some() {
            return Err(AgentCreateError::AgentAlreadyExists(id));
        }
        self.register_agent(id, state)
    }

    pub(super) fn register_restored_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), AgentCreateError> {
        self.register_agent(id, state)
    }

    fn register_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentState>,
    ) -> Result<(), AgentCreateError> {
        self.persistence
            .collection::<AgentState>(AGENT_STATE_NAME)?
            .register(&agent_instance(id), state)?;
        Ok(())
    }
}
