//! Resolves a baked manifest and assembles the corresponding [`Agent`](super::Agent).

mod construction;
mod create;
mod error;
mod layout;
mod long_term;
mod persistence;

use std::marker::PhantomData;
use std::sync::Arc;

use crate::config::SharedApiManager;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::ProfileStore;
use claw_persistence::SharedPersistence;
use claw_skill::FsSkillRegistry;
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
pub(crate) struct AgentManager<
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
> {
    persistence: SharedPersistence<Filesystem>,
    api_manager: SharedApiManager,
    tool_registry: Arc<ToolRegistry>,
    _http: PhantomData<fn() -> Http>,
    _timer: PhantomData<fn() -> Timer>,
    transcript_dir: String,
    long_term: LongTermDeps<Filesystem>,
    profile_store: ProfileStore<Filesystem>,
    skill_registry: Arc<FsSkillRegistry<Filesystem>>,
}
