use std::sync::Arc;

use claw_api::{RetryPolicy, ToolCall};
use claw_context::{Block, BlockKind};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::{Transcript, TranscriptStore};
use claw_permission::PermissionPolicy;
use claw_tool::ToolGroup;

use crate::agent::baked;
use crate::agent::base_agent::{agent_effect_channel, BaseAgent, BaseAgentConfig, ContextAdapter};
use crate::agent::context_adapters::{
    AgentModeContextAdapter, AgentResumeNotice, ConversationHistoryContextAdapter,
    ProfileContextAdapter, ReasoningEffortContextAdapter, ResumedContextAdapter,
    SkillContextAdapter,
};
use crate::agent::tools::internal_tools;
use crate::agent::{AgentKind, AgentState, ReasoningEffort, ReasoningEffortHandle};
use crate::config::ApiUsage;

use super::error::AgentCreateError;
use super::{AgentId, AgentManager};

const COMPACTION_TRIGGER_TOKENS: usize = 6000;
const COMPACTION_KEEP_RECENT_TOKENS: usize = 2000;
const COMPACTION_SEGMENT_TOKEN_BUDGET: usize = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceConfig {
    InMemory,
    Persistent,
}

struct AgentEnvironment {
    transcript: Box<dyn Transcript>,
    is_root: bool,
    permission_policy: Arc<dyn PermissionPolicy>,
    extension_tools: Vec<ToolGroup>,
    inherited_context: Vec<Block<'static>>,
    reasoning_effort: ReasoningEffort,
    state: Option<AgentState>,
    resume_notice: Option<AgentResumeNotice>,
}

pub(crate) struct AdditionalAgentState {
    inflight_toolcalls: Vec<ToolCall>,
}

impl AdditionalAgentState {
    pub(crate) fn new(inflight_toolcalls: Vec<ToolCall>) -> Self {
        Self { inflight_toolcalls }
    }

    fn into_resume_notice(self) -> AgentResumeNotice {
        AgentResumeNotice::new(self.inflight_toolcalls)
    }
}

impl<
        Filesystem: ClawFs + 'static,
        Http: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    > AgentManager<Filesystem, Http, Timer>
{
    pub(crate) fn fork_from(
        &self,
        id: AgentId,
        kind: &AgentKind,
        is_root: bool,
        permission_policy: Arc<dyn PermissionPolicy + 'static>,
        reasoning_effort: ReasoningEffort,
        persistence_config: PersistenceConfig,
        extension_tools: Vec<ToolGroup>,
        agent: &BaseAgent<Http, Timer>,
    ) -> Result<(BaseAgent<Http, Timer>, ReasoningEffortHandle), AgentCreateError> {
        let transcript = self.open_transcript(id, kind, persistence_config)?;
        let (agent, reasoning_effort_handle) = self.create_agent(
            id,
            kind,
            AgentEnvironment {
                transcript,
                is_root,
                permission_policy,
                extension_tools,
                inherited_context: agent.context().fork_blocks(),
                reasoning_effort,
                state: None,
                resume_notice: None,
            },
        )?;
        if persistence_config == PersistenceConfig::Persistent {
            self.register_new_agent(id, agent.recovery_state())?;
        }
        Ok((agent, reasoning_effort_handle))
    }

    pub(crate) fn resume_from(
        &self,
        id: AgentId,
        is_root: bool,
        permission_policy: Arc<dyn PermissionPolicy + 'static>,
        reasoning_effort: ReasoningEffort,
        extension_tools: Vec<ToolGroup>,
        additional_state: Option<AdditionalAgentState>,
    ) -> Result<(BaseAgent<Http, Timer>, ReasoningEffortHandle), AgentCreateError> {
        let persisted = self.load_persisted_agent(id)?;
        let kind = persisted.kind();
        let transcript = self.open_transcript(id, &kind, PersistenceConfig::Persistent)?;
        let (agent, reasoning_effort_handle) = self.create_agent(
            id,
            &kind,
            AgentEnvironment {
                transcript,
                is_root,
                permission_policy,
                extension_tools,
                inherited_context: Vec::new(),
                reasoning_effort,
                state: Some(persisted),
                resume_notice: additional_state.map(AdditionalAgentState::into_resume_notice),
            },
        )?;
        self.register_restored_agent(id, agent.recovery_state())?;
        Ok((agent, reasoning_effort_handle))
    }

    pub(crate) fn create(
        &self,
        id: AgentId,
        kind: &AgentKind,
        is_root: bool,
        permission_policy: Arc<dyn PermissionPolicy + 'static>,
        reasoning_effort: ReasoningEffort,
        persistence_config: PersistenceConfig,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(BaseAgent<Http, Timer>, ReasoningEffortHandle), AgentCreateError> {
        let transcript = self.open_transcript(id, kind, persistence_config)?;
        let (agent, reasoning_effort_handle) = self.create_agent(
            id,
            kind,
            AgentEnvironment {
                transcript,
                is_root,
                permission_policy,
                extension_tools,
                inherited_context: Vec::new(),
                reasoning_effort,
                state: None,
                resume_notice: None,
            },
        )?;
        if persistence_config == PersistenceConfig::Persistent {
            self.register_new_agent(id, agent.recovery_state())?;
        }
        Ok((agent, reasoning_effort_handle))
    }

    fn open_transcript(
        &self,
        id: AgentId,
        kind: &AgentKind,
        persistence: PersistenceConfig,
    ) -> Result<Box<dyn Transcript>, AgentCreateError> {
        match persistence {
            PersistenceConfig::Persistent => {
                TranscriptStore::<Filesystem>::new(id.0, &self.transcript_dir)
                    .map(|store| Box::new(store) as Box<dyn Transcript>)
                    .map_err(|error| {
                        tracing::error!(
                            name: "transcript_open_failed",
                            agent = %id,
                            kind = %kind.as_str(),
                        );
                        AgentCreateError::Transcript(error)
                    })
            }
            PersistenceConfig::InMemory => {
                Ok(Box::new(TranscriptStore::<Filesystem>::in_memory(id.0)))
            }
        }
    }

    /// Build one stopped agent of `kind`.
    ///
    /// Its owner supplies storage, inherited context, and any extension tools
    /// through `environment`. The manager does not interpret orchestration roles.
    ///
    /// # Errors
    ///
    /// Returns a typed error when `kind` is unknown or the agent cannot be
    /// assembled; callers decide where to render it for logs or user-facing
    /// errors.
    fn create_agent(
        &self,
        id: AgentId,
        kind: &AgentKind,
        environment: AgentEnvironment,
    ) -> Result<(BaseAgent<Http, Timer>, ReasoningEffortHandle), AgentCreateError> {
        let span = tracing::info_span!("agent.create");
        let _enter = span.enter();
        let AgentEnvironment {
            transcript,
            is_root,
            permission_policy,
            extension_tools,
            inherited_context,
            reasoning_effort,
            state: resume,
            resume_notice,
        } = environment;

        let manifest = baked::find(kind).ok_or_else(|| {
            tracing::error!(name: "unknown_kind", kind = %kind.as_str());
            AgentCreateError::UnknownKind(kind.as_str().to_owned())
        })?;
        let runtime = manifest.runtime();
        let skills = self.skills.skill_set();
        let (mode_state, resumed_state) = match resume {
            Some(state) => {
                let (mode, resumed) = state.into_parts();
                (Some(mode), Some(resumed))
            }
            None => (None, None),
        };
        // The per-kind blacklist stays attached to this ToolSet projection so
        // registry refreshes and later local groups follow the same exact-name
        // policy.
        let mut tools = self.tools.tool_set_with_blacklist(runtime.tool_blacklist());
        let (effect_emitter, effect_inbox) = agent_effect_channel();
        tools.add_group(internal_tools(effect_emitter.clone()))?;
        for extension in extension_tools {
            tools.add_group(extension)?;
        }
        let resumed_adapter =
            ResumedContextAdapter::new(resumed_state, resume_notice, tools.discovery());

        // Only `BaseAgent` holds the transcript (as `dyn Transcript`); context
        // adapters read it through the `&dyn Transcript` lent to `prepare`.
        let conversation_history =
            ConversationHistoryContextAdapter::with_llm_compaction::<Http, Timer>(
                Arc::clone(&self.api_manager),
                COMPACTION_TRIGGER_TOKENS,
                COMPACTION_KEEP_RECENT_TOKENS,
                COMPACTION_SEGMENT_TOKEN_BUDGET,
            );
        let profile_adapter = ProfileContextAdapter::new(self.profile.clone());
        let adapter = match self.long_term.adapter(kind.as_str()) {
            Ok(adapter) => adapter,
            Err(error) => {
                tracing::error!(
                    name: "context_adapter_attach_failed",
                    agent = %id,
                    adapter = "long_term",
                    kind = %kind.as_str(),
                );
                return Err(AgentCreateError::LongTerm(error));
            }
        };
        // AgentManager is the only configured-agent assembly point. BaseAgent sees
        // one generic, immutable adapter set; concrete mode, memory, and skill
        // semantics do not leak into its runtime protocol. ResumedContextAdapter
        // is the boundary that contributes resume context and exposes the pure
        // discovery group implemented alongside the resumed adapter.
        let (reasoning_effort_adapter, reasoning_effort_handle) =
            ReasoningEffortContextAdapter::new(reasoning_effort);
        let context_adapters: Vec<Box<dyn ContextAdapter>> = vec![
            Box::new(AgentModeContextAdapter::new(mode_state, effect_emitter)),
            Box::new(reasoning_effort_adapter),
            Box::new(resumed_adapter),
            Box::new(conversation_history),
            Box::new(SkillContextAdapter::new(skills)),
            Box::new(profile_adapter),
            Box::new(adapter),
        ];
        let api_usage = if is_root {
            ApiUsage::RootAgent
        } else {
            ApiUsage::SubAgent
        };
        let base_config = BaseAgentConfig {
            kind: kind.clone(),
            transcript: transcript,
            api_manager: Arc::clone(&self.api_manager),
            api_usage,
            tools,
            effect_inbox,
            permission_policy,
            agent_instruction: Block::new(
                BlockKind::AgentInstruction,
                runtime.instructions().trim().to_owned(),
            ),
            inherited_context,
            context_adapters,
            retry_policy: RetryPolicy::new(runtime.retries()),
        };
        let agent = BaseAgent::<Http, Timer>::build(base_config)?;

        tracing::info!(name: "created", agent = %id, kind = %kind.as_str());
        Ok((agent, reasoning_effort_handle))
    }
}
