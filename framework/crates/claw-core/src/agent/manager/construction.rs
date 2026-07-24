use std::marker::PhantomData;
use std::sync::Arc;

use crate::config::SharedApiManager;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::ProfileStore;
use claw_persistence::SharedPersistence;
use claw_skill::{FsSkillRegistry, SkillError};
use claw_tool::ToolRegistry;

use super::error::AgentManagerError;
use super::layout::AgentManagerLayout;
use super::long_term::LongTermDeps;
use super::AgentManager;

impl<
        Filesystem: ClawFs + 'static,
        Http: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    > AgentManager<Filesystem, Http, Timer>
{
    /// The manager owns the memory layout below `persistence_dir`: transcripts,
    /// editable profile documents, and long-term memory. `Filesystem` selects
    /// the static filesystem HAL backend used by those stores.
    ///
    /// # Errors
    ///
    /// Returns [`AgentManagerError::MissingPersistenceDir`] when the
    /// persistence root is blank.
    pub(crate) fn new(
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        memory_directory: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
    ) -> Result<Self, AgentManagerError> {
        let span = tracing::info_span!("agent.manager");
        let _enter = span.enter();
        if memory_directory.trim().is_empty() {
            log::error!("Agent manager persistence directory is empty");
            tracing::error!(name: "missing_persistence_dir", reason = "empty");
            return Err(AgentManagerError::MissingPersistenceDir);
        }
        let layout = AgentManagerLayout::new(memory_directory);

        let long_term = match LongTermDeps::<Filesystem>::from_root::<Http, Timer>(
            &layout.long_term_dir,
            Arc::clone(&api_manager),
        ) {
            Ok(deps) => deps,
            Err(error) => {
                log::error!("long-term memory initialization failed: {error}");
                tracing::error!(name: "long_term_memory_init_failed", kind = "init");
                return Err(error.into());
            }
        };

        let profile_store = ProfileStore::new(&layout.profile_dir);
        let skill_registry = build_skill_registry::<Filesystem>(skill_roots)?;

        let manager = Self {
            persistence,
            api_manager,
            tool_registry,
            _http: PhantomData,
            _timer: PhantomData,
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
fn build_skill_registry<F: ClawFs + 'static>(
    skill_roots: Vec<String>,
) -> Result<Arc<FsSkillRegistry<F>>, SkillError> {
    let span = tracing::info_span!("skill.catalog");
    let _enter = span.enter();
    let mut registry = FsSkillRegistry::<F>::new();
    for root in skill_roots {
        if !F::exists(root.as_str()) {
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
