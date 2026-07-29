//! Concrete providers driven by [`BaseAgent`](super::BaseAgent).
//!
//! BaseAgent owns the generic
//! [`ContextProvider`](super::base_agent::ContextProvider) port. Domain behavior
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

pub(in crate::agent) use agent_mode::{AgentMode, AgentModeContextProvider};
pub(in crate::agent) use conversation_history::ConversationHistoryContextProvider;
pub(in crate::agent) use long_term_memory::LongTermMemoryContextProvider;
pub(in crate::agent) use profile::ProfileContextProvider;
pub use reasoning_effort::ReasoningEffort;
pub(crate) use reasoning_effort::{ReasoningEffortContextProvider, ReasoningEffortHandle};
pub(in crate::agent) use resume::ResumeContextProvider;
pub(in crate::agent) use skill::SkillContextProvider;
