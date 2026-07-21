//! Resolves a baked agent manifest and assembles the corresponding [`BaseAgent`](super::BaseAgent).

mod construction;
mod create;
mod environment;
mod error;
mod layout;
mod long_term;
mod transcript;

use std::marker::PhantomData;
use std::sync::Arc;

use crate::config::SharedApiManager;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::ProfileStore;
use claw_skill::FsSkillRegistry;
use claw_tool::ToolRegistry;

use self::long_term::LongTermDeps;

pub(crate) use environment::{AgentEnvironment, AgentResume, TranscriptTarget};
pub(crate) use error::{FsAgentCreateError, FsAgentFactoryError};

/// Shared assembly dependencies for independently-built agents.
pub(crate) struct FsAgentFactory<
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
> {
    api_manager: SharedApiManager,
    tools: Arc<ToolRegistry>,
    _http: PhantomData<fn() -> Http>,
    _timer: PhantomData<fn() -> Timer>,
    transcript_dir: String,
    long_term: LongTermDeps<Filesystem>,
    profile: ProfileStore<Filesystem>,
    skills: Arc<FsSkillRegistry<Filesystem>>,
}
