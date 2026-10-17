//! Resolves a baked manifest and assembles the corresponding [`Agent`](super::Agent).

mod construction;
mod create;
mod error;
mod layout;
mod long_term;
mod persistence;
mod transcript;

use alloc::{
    collections::BTreeMap,
    rc::{Rc, Weak},
    string::String,
    vec::Vec,
};
use core::cell::RefCell;
use portable_atomic_util::Arc;

use crate::config::SharedApiManager;
use barracuda_agent_memory::{ProfileStore, Transcript, TranscriptStore, TransientTranscript};
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
pub use transcript::{TranscriptEntry, TranscriptRole};

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
    /// In-memory Agents' transcripts, while their Agent holds them.
    transient_transcripts: RefCell<BTreeMap<AgentId, Weak<TransientTranscript>>>,
    pending_transcript_deletes: RefCell<Vec<AgentId>>,
}

impl<Tcp, Resolver> AgentManager<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    /// The last `limit` messages of Agent `id`'s conversation as a person
    /// reads it, each cut to `max_chars` characters, or `None` when the
    /// Agent has no transcript here.
    #[must_use]
    pub fn transcript(
        &self,
        id: AgentId,
        limit: usize,
        max_chars: usize,
    ) -> Option<Vec<TranscriptEntry>> {
        let turns = if let Some(store) = self.transcripts.borrow().get(&id) {
            store.turns()
        } else {
            self.transient_transcripts
                .borrow()
                .get(&id)
                .and_then(Weak::upgrade)?
                .turns()
        };
        Some(transcript::entries(&turns, limit, max_chars))
    }
}
