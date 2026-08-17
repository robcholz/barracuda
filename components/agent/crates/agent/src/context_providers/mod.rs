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
mod tool_discovery;

pub(crate) use agent_mode::AgentModeContextProvider;
pub(crate) use conversation_history::ConversationHistoryContextProvider;
pub(crate) use long_term_memory::LongTermMemoryContextProvider;
pub(crate) use profile::ProfileContextProvider;
pub use reasoning_effort::ReasoningEffort;
pub(crate) use reasoning_effort::ReasoningEffortContextProvider;
pub use reasoning_effort::ReasoningEffortHandle;
pub(crate) use resume::ResumeContextProvider;
pub(crate) use skill::SkillContextProvider;
pub(crate) use tool_discovery::ToolDiscoveryContextProvider;
