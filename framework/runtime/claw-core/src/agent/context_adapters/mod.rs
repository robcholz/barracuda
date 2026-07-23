//! Concrete adapters driven by [`BaseAgent`](super::BaseAgent).
//!
//! BaseAgent owns the generic
//! [`ContextAdapter`](super::base_agent::ContextAdapter) port. Domain behavior
//! such as agent mode, resume context, conversation projection, skills,
//! profile, and long-term memory lives here as implementations.

mod agent_mode;
mod async_llm;
mod conversation_history;
mod long_term_memory;
mod profile;
mod reasoning_effort;
mod resume;
mod skill;

pub(in crate::agent) use agent_mode::AgentModeContextAdapter;
pub(in crate::agent) use conversation_history::ConversationHistoryContextAdapter;
pub(in crate::agent) use long_term_memory::LongTermMemoryContextAdapter;
pub(in crate::agent) use profile::ProfileContextAdapter;
pub use reasoning_effort::ReasoningEffort;
pub(crate) use reasoning_effort::{ReasoningEffortContextAdapter, ReasoningEffortHandle};
pub(in crate::agent) use resume::ResumeContextAdapter;
pub(in crate::agent) use skill::SkillContextAdapter;
