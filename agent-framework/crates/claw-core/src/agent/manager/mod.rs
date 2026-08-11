//! Resolves a baked manifest and assembles the corresponding [`Agent`](super::Agent).

mod construction;
mod create;
mod error;
mod layout;
mod long_term;
mod persistence;

use alloc::{string::String, sync::Arc};

use crate::config::SharedApiManager;
use claw_api::ClawApiFactory;
use claw_interface::ClawFs;
use claw_memory::ProfileStore;
use claw_net::{Dns, TcpConnect};
use claw_persistence::SharedPersistence;
use claw_skill::SkillRegistry;
use claw_tool::ToolRegistry;

use self::long_term::LongTermDeps;
pub(crate) use create::PersistenceConfig;
pub use error::AgentCreateError;
pub(crate) use error::AgentManagerError;

crate::define_prefixed_id!(AgentId, "agent-", "agent");
crate::define_id_allocator!(
    /// Hands out process-unique agent ids for the current runtime.
    pub(crate) AgentIdAllocator(AgentId),
    AgentId(1)
);

/// Shared assembly dependencies for independently-built agents.
pub(crate) struct AgentManager<Filesystem: ClawFs + 'static, Http: TcpConnect + Dns + 'static> {
    filesystem: Arc<Filesystem>,
    persistence: SharedPersistence<Filesystem>,
    api_manager: SharedApiManager,
    tool_registry: Arc<ToolRegistry>,
    llm_factory: ClawApiFactory<Http>,
    transcript_dir: String,
    long_term: LongTermDeps<Filesystem>,
    profile_store: ProfileStore<Filesystem>,
    skill_registry: Arc<dyn SkillRegistry>,
}
