//! Resolves a baked manifest and assembles the corresponding [`Agent`](super::Agent).

mod construction;
mod create;
mod error;
mod layout;
mod long_term;
mod persistence;

use alloc::{collections::BTreeMap, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;
use portable_atomic_util::Arc;

use crate::config::SharedApiManager;
use barracuda_agent_memory::{ProfileStore, TranscriptStore};
use barracuda_agent_persistence::{DurableState, SharedPersistence};
use barracuda_agent_skill::SkillRegistry;
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::ModelApiFactory;
use barracuda_vfs::ScopedVfs;
use http_client::embedded_nal_async::{Dns, TcpConnect};

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
pub struct AgentManager<Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    filesystem: ScopedVfs,
    persistence: SharedPersistence,
    api_manager: SharedApiManager,
    tool_registry: Arc<ToolRegistry>,
    llm_factory: ModelApiFactory<Tcp, Resolver>,
    transcript_dir: String,
    long_term: LongTermDeps,
    profile_store: ProfileStore,
    skill_registry: Arc<dyn SkillRegistry>,
    persisted_agents: RefCell<BTreeMap<AgentId, DurableState<crate::AgentEngineState>>>,
    transcripts: RefCell<BTreeMap<AgentId, Rc<TranscriptStore>>>,
    pending_transcript_deletes: RefCell<Vec<AgentId>>,
}
