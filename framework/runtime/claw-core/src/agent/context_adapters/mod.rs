//! Concrete adapters driven by [`BaseAgent`](super::BaseAgent).
//!
//! BaseAgent owns the generic
//! [`ContextAdapter`](super::base_agent::ContextAdapter) port. Domain behavior
//! such as agent mode, conversation projection, skills, profile, and long-term
//! memory lives here as implementations.

mod agent_mode;
mod async_llm;
mod conversation_history;
mod long_term_memory;
mod profile;
mod skill;

pub(crate) use agent_mode::{AgentMode, AgentModeContextAdapter};
pub(crate) use conversation_history::{
    CompactionPolicy, ConversationHistoryContextAdapter, LlmCompactor,
};
pub(crate) use long_term_memory::{
    agent_store, global_store, Extractor, LlmExtractor, LongTermMemoryContextAdapter,
};
pub(crate) use profile::ProfileContextAdapter;
pub(crate) use skill::SkillContextAdapter;
