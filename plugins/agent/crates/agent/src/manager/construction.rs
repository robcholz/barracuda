use alloc::{boxed::Box, collections::BTreeMap, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;
use portable_atomic_util::Arc;

use crate::config::SharedApiManager;
use barracuda_agent_memory::ProfileStore;
use barracuda_agent_memory::TranscriptStore;
use barracuda_agent_persistence::{DurableState, SharedPersistence};
use barracuda_agent_skill::SkillRegistry;
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::ModelApiFactory;
use barracuda_vfs::ScopedVfs;
use http_client::embedded_nal_async::{Dns, TcpConnect};

use barracuda_agent_skill::{FsSkillRegistry, SkillError};

use super::error::AgentManagerError;
use super::layout::AgentManagerLayout;
use super::long_term::LongTermDeps;
use super::AgentManager;

impl<Tcp, Resolver> AgentManager<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    /// The manager owns the memory layout below `persistence_dir`: transcripts,
    /// editable profile documents, and long-term memory. All durable stores
    /// explicitly share the supplied filesystem instance.
    ///
    /// # Errors
    ///
    /// Returns [`AgentManagerError::MissingPersistenceDir`] when the
    /// persistence root is blank.
    pub async fn new(
        filesystem: ScopedVfs,
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence,
        memory_directory: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
    ) -> Result<Self, AgentManagerError> {
        let span = tracing::info_span!("agent.manager");
        let _enter = span.enter();
        if memory_directory.trim().is_empty() {
            log::error!("Agent manager persistence directory is empty");
            tracing::error!(name: "missing_persistence_dir", reason = "empty");
            return Err(AgentManagerError::MissingPersistenceDir);
        }
        let layout = AgentManagerLayout::new(memory_directory);

        let long_term = match LongTermDeps::from_root(
            filesystem.clone(),
            &layout.long_term_dir,
            crate::baked::entries()
                .iter()
                .map(|entry| entry.kind().as_str()),
            Arc::clone(&api_manager),
            llm_factory.clone(),
        )
        .await
        {
            Ok(deps) => deps,
            Err(error) => {
                log::error!("long-term memory initialization failed: {error}");
                tracing::error!(name: "long_term_memory_init_failed", kind = "init");
                return Err(error.into());
            }
        };

        let profile_store = ProfileStore::new(filesystem.clone(), &layout.profile_dir);
        let skill_registry: Arc<dyn SkillRegistry> =
            build_fs_skill_registry(filesystem.clone(), skill_roots).await?;

        let agent_states = persistence
            .collection::<crate::AgentEngineState>("agents")
            .map_err(super::AgentCreateError::from)?;
        let mut persisted_agents = BTreeMap::new();
        let mut transcripts = BTreeMap::new();
        for instance in agent_states
            .list()
            .await
            .map_err(super::AgentCreateError::from)?
        {
            let id = super::AgentId::from_wire(instance.as_str()).map_err(|_| {
                super::AgentCreateError::InvalidPersistedAgentId(instance.as_str().into())
            })?;
            let value = agent_states
                .load(&instance)
                .await
                .map_err(super::AgentCreateError::from)?
                .ok_or(super::AgentCreateError::AgentNotFound(id))?;
            let state = DurableState::new(value);
            agent_states
                .register(&instance, &state)
                .map_err(super::AgentCreateError::from)?;
            let transcript = TranscriptStore::new(filesystem.clone(), id.0, &layout.transcript_dir)
                .await
                .map_err(super::AgentCreateError::from)?;
            persisted_agents.insert(id, state);
            transcripts.insert(id, Rc::new(transcript));
        }

        let manager = Self {
            filesystem,
            persistence,
            api_manager,
            tool_registry,
            llm_factory,
            transcript_dir: layout.transcript_dir,
            long_term,
            profile_store,
            skill_registry,
            persisted_agents: RefCell::new(persisted_agents),
            transcripts: RefCell::new(transcripts),
            pending_transcript_deletes: RefCell::new(Vec::new()),
        };
        manager.purge_dead().await?;
        Ok(manager)
    }
}

/// Build the shared, globally unique skill catalog from `skill_roots`.
///
/// A missing root scans as empty but stays registered, so skills installed
/// there later appear on `skill_reload`. A malformed or duplicated package is
/// left out with a warning; only a root that cannot be listed aborts
/// construction, since user skills share a writable Workspace directory.
async fn build_fs_skill_registry(
    filesystem: ScopedVfs,
    skill_roots: Vec<String>,
) -> Result<Arc<dyn SkillRegistry>, SkillError> {
    let span = tracing::info_span!("skill.catalog");
    let _enter = span.enter();
    let mut registry = FsSkillRegistry::new(filesystem.clone());
    for root in skill_roots {
        if !filesystem
            .exists(root.as_str())
            .await
            .map_err(|error| SkillError::ScanFailed(root.clone(), error))?
        {
            log::info!("skill catalog root {root} is empty until it is created");
            tracing::info!(name: "root_missing", "");
        }
        match registry.add_root(root).await {
            Ok(next) => registry = next,
            Err(error) => {
                log::warn!("skill catalog scan failed: {error}");
                tracing::warn!(name: "scan_failed", kind = "add_root");
                return Err(error);
            }
        }
    }
    for rejected in registry.catalog().rejected() {
        log::warn!("skill package left out of the catalog: {rejected}");
        tracing::warn!(name: "skill_rejected", "");
    }
    Ok(Arc::from(Box::new(registry) as Box<dyn SkillRegistry>))
}
