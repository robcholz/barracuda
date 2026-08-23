use alloc::{string::String, sync::Arc, vec::Vec};

use crate::config::SharedApiManager;
use barracuda_agent_memory::ProfileStore;
use barracuda_agent_persistence::SharedPersistence;
use barracuda_agent_skill::SkillRegistry;
use barracuda_agent_tool::ToolRegistry;
use barracuda_fs::FileSystem;
use barracuda_model_api::ModelApiFactory;
use barracuda_net::{Dns, TcpConnect};

use barracuda_agent_skill::{FsSkillRegistry, SkillError};

use super::error::AgentManagerError;
use super::layout::AgentManagerLayout;
use super::long_term::LongTermDeps;
use super::AgentManager;

impl<Filesystem: FileSystem + 'static, Http: TcpConnect + Dns + 'static>
    AgentManager<Filesystem, Http>
{
    /// The manager owns the memory layout below `persistence_dir`: transcripts,
    /// editable profile documents, and long-term memory. All durable stores
    /// explicitly share the supplied filesystem instance.
    ///
    /// # Errors
    ///
    /// Returns [`AgentManagerError::MissingPersistenceDir`] when the
    /// persistence root is blank.
    pub fn new(
        filesystem: Arc<Filesystem>,
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        memory_directory: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<Http>,
    ) -> Result<Self, AgentManagerError> {
        let span = tracing::info_span!("agent.manager");
        let _enter = span.enter();
        if memory_directory.trim().is_empty() {
            log::error!("Agent manager persistence directory is empty");
            tracing::error!(name: "missing_persistence_dir", reason = "empty");
            return Err(AgentManagerError::MissingPersistenceDir);
        }
        let layout = AgentManagerLayout::new(memory_directory);

        let long_term = match LongTermDeps::<Filesystem>::from_root::<Http>(
            Arc::clone(&filesystem),
            &layout.long_term_dir,
            Arc::clone(&api_manager),
            llm_factory.clone(),
        ) {
            Ok(deps) => deps,
            Err(error) => {
                log::error!("long-term memory initialization failed: {error}");
                tracing::error!(name: "long_term_memory_init_failed", kind = "init");
                return Err(error.into());
            }
        };

        let profile_store = ProfileStore::new(Arc::clone(&filesystem), &layout.profile_dir);
        let skill_registry: Arc<dyn SkillRegistry> =
            build_fs_skill_registry(Arc::clone(&filesystem), skill_roots)?;

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
        };
        manager.purge_dead()?;
        Ok(manager)
    }
}

/// Build the shared skill catalog from the priority-ordered `skill_roots`.
///
/// A missing root is skipped so the agent still starts; a real scan failure
/// (e.g. a malformed `SKILL.md`) aborts construction.
fn build_fs_skill_registry<F: FileSystem + 'static>(
    filesystem: Arc<F>,
    skill_roots: Vec<String>,
) -> Result<Arc<FsSkillRegistry<F>>, SkillError> {
    let span = tracing::info_span!("skill.catalog");
    let _enter = span.enter();
    let mut registry = FsSkillRegistry::new(Arc::clone(&filesystem));
    for root in skill_roots {
        if !filesystem.exists(root.as_str()) {
            log::warn!("skill catalog root is missing: {root}");
            tracing::warn!(name: "root_missing", "");
            continue;
        }
        match registry.set_root(root) {
            Ok(next) => registry = next,
            Err(error) => {
                log::warn!("skill catalog scan failed: {error}");
                tracing::warn!(name: "scan_failed", kind = "set_root");
                return Err(error);
            }
        }
    }
    Ok(Arc::new(registry))
}
