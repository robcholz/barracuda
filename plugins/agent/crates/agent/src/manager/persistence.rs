use alloc::{collections::BTreeSet, vec::Vec};

use barracuda_agent_memory::TranscriptStore;
use barracuda_agent_persistence::{DurableState, InstanceId};

use super::AgentId;
use crate::AgentEngineState;

use super::error::AgentCreateError;
use super::AgentManager;
use http_client::embedded_nal_async::{Dns, TcpConnect};

const AGENT_STATE_NAME: &str = "agents";

fn agent_instance(id: AgentId) -> Result<InstanceId, AgentCreateError> {
    InstanceId::new(id.to_wire()).map_err(AgentCreateError::from)
}

impl<Tcp, Resolver> AgentManager<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    /// Delete transcript files whose owning Agent record no longer exists.
    pub(super) async fn purge_dead(&self) -> Result<(), AgentCreateError> {
        let agents = self
            .list_persisted_agents()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        for transcript in
            TranscriptStore::list_persisted_ids(&self.filesystem, &self.transcript_dir).await?
        {
            let agent = AgentId::new(transcript);
            if !agents.contains(&agent) {
                TranscriptStore::delete(&self.filesystem, transcript, &self.transcript_dir).await?;
            }
        }
        Ok(())
    }

    pub fn list_persisted_agents(&self) -> Result<Vec<AgentId>, AgentCreateError> {
        Ok(self.persisted_agents.borrow().keys().copied().collect())
    }

    /// Remove any canonical storage owned by `id`.
    ///
    /// Missing state and transcript files are treated as already removed, so
    /// callers use the same lifecycle path for persistent and in-memory Agents.
    /// Every live Agent handle for `id` must be dropped before calling this.
    pub fn remove(&self, id: AgentId) -> Result<(), AgentCreateError> {
        self.persistence
            .collection::<AgentEngineState>(AGENT_STATE_NAME)?
            .remove(&agent_instance(id)?)?;
        self.persisted_agents.borrow_mut().remove(&id);
        self.transcripts.borrow_mut().remove(&id);
        self.pending_transcript_deletes.borrow_mut().push(id);
        Ok(())
    }

    pub(super) fn load_persisted_agent(
        &self,
        id: AgentId,
    ) -> Result<DurableState<AgentEngineState>, AgentCreateError> {
        self.persisted_agents
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(AgentCreateError::AgentNotFound(id))
    }

    pub(super) fn register_new_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentEngineState>,
    ) -> Result<(), AgentCreateError> {
        if self.persisted_agents.borrow().contains_key(&id) {
            return Err(AgentCreateError::AgentAlreadyExists(id));
        }
        self.register_agent(id, state)?;
        self.persisted_agents.borrow_mut().insert(id, state.clone());
        Ok(())
    }

    pub(super) fn register_restored_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentEngineState>,
    ) -> Result<(), AgentCreateError> {
        self.register_agent(id, state)
    }

    fn register_agent(
        &self,
        id: AgentId,
        state: &DurableState<AgentEngineState>,
    ) -> Result<(), AgentCreateError> {
        self.persistence
            .collection::<AgentEngineState>(AGENT_STATE_NAME)?
            .register(&agent_instance(id)?, state)?;
        Ok(())
    }

    /// Applies transcript deletions queued by synchronous actor lifecycle code.
    pub async fn flush_storage(&self) -> Result<(), AgentCreateError> {
        let pending = core::mem::take(&mut *self.pending_transcript_deletes.borrow_mut());
        for (index, id) in pending.iter().copied().enumerate() {
            if let Err(error) =
                TranscriptStore::delete(&self.filesystem, id.0, &self.transcript_dir).await
            {
                if let Some(remaining) = pending.get(index..) {
                    self.pending_transcript_deletes
                        .borrow_mut()
                        .extend_from_slice(remaining);
                }
                return Err(error.into());
            }
        }
        Ok(())
    }
}
