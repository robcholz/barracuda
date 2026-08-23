//! Resolves a baked manifest and assembles the corresponding [`Agent`](super::Agent).

mod construction;
mod create;
mod error;
mod layout;
mod long_term;
mod persistence;

use alloc::{string::String, sync::Arc};

use crate::config::SharedApiManager;
use barracuda_agent_memory::ProfileStore;
use barracuda_agent_persistence::SharedPersistence;
use barracuda_agent_skill::SkillRegistry;
use barracuda_agent_tool::ToolRegistry;
use barracuda_fs::FileSystem;
use barracuda_model_api::ModelApiFactory;
use barracuda_net::{Dns, TcpConnect};

use self::long_term::LongTermDeps;
pub use create::PersistenceConfig;
pub use error::AgentCreateError;
pub use error::AgentManagerError;

crate::define_prefixed_id!(AgentId, "agent-", "agent");
crate::define_id_allocator!(
    /// Hands out process-unique agent ids for the current runtime.
    pub AgentIdAllocator(AgentId),
    AgentId(1)
);

/// Shared assembly dependencies for independently-built agents.
pub struct AgentManager<Filesystem: FileSystem + 'static, Http: TcpConnect + Dns + 'static> {
    filesystem: Arc<Filesystem>,
    persistence: SharedPersistence<Filesystem>,
    api_manager: SharedApiManager,
    tool_registry: Arc<ToolRegistry>,
    llm_factory: ModelApiFactory<Http>,
    transcript_dir: String,
    long_term: LongTermDeps<Filesystem>,
    profile_store: ProfileStore<Filesystem>,
    skill_registry: Arc<dyn SkillRegistry>,
}
