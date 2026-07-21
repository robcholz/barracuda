use std::sync::Arc;

use claw_context::{Block, BlockKind};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::TranscriptStore;

use crate::agent::base_agent::{AgentCommand, BaseAgent, BaseAgentConfig, ContextAdapter};
use crate::agent::config::{AgentConfig, AgentConfigError};
use crate::agent::context_adapters::{
    AgentModeContextAdapter, AgentResumeNotice, ConversationHistoryContextAdapter,
    ProfileContextAdapter, ResumedContextAdapter, SkillContextAdapter,
};
use crate::agent::effect::agent_effect_channel;
use crate::agent::tools::internal_tools;
use crate::config::catalog as agent_catalog;
use crate::protocol::{AgentId, AgentKind, Message};

use super::error::FsAgentCreateError;
use super::{AgentEnvironment, FsAgentFactory};

const COMPACTION_TRIGGER_TOKENS: usize = 6000;
const COMPACTION_KEEP_RECENT_TOKENS: usize = 2000;
const COMPACTION_SEGMENT_TOKEN_BUDGET: usize = 1500;

impl<
        Filesystem: ClawFs + 'static,
        Http: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    > FsAgentFactory<Filesystem, Http, Timer>
{
    /// Build one agent of `kind`, already tasked with `goal`.
    ///
    /// Its owner supplies storage, inherited context, and any extension tools
    /// through `environment`. The factory does not interpret orchestration roles.
    ///
    /// # Errors
    ///
    /// Returns a typed error when `kind` is unknown or the agent cannot be
    /// assembled; callers decide where to render it for logs or user-facing
    /// errors.
    pub(crate) fn create_agent(
        &self,
        id: AgentId,
        kind: &AgentKind,
        goal: Message,
        environment: AgentEnvironment,
    ) -> Result<BaseAgent<Http, Timer>, FsAgentCreateError> {
        let span = tracing::info_span!("agent.create");
        let _enter = span.enter();
        // The config is pure baked data. The per-kind blacklist stays attached
        // to this ToolSet projection so registry refreshes and later local
        // groups follow the same exact-name policy.
        let config = self.resolve_config(kind).map_err(|error| {
            match &error {
                AgentConfigError::UnknownKind(_) => {
                    tracing::error!(name: "unknown_kind", kind = %kind.as_str());
                }
            }
            FsAgentCreateError::Config(error)
        })?;
        let (mode_state, resumed_state, resume_notice) = match environment.resume {
            Some(resume) => {
                let (state, legacy_inflight_toolcalls) = resume.into_parts();
                let (mode, resumed) = state.into_parts();
                let notice = AgentResumeNotice::new(legacy_inflight_toolcalls);
                (Some(mode), Some(resumed), Some(notice))
            }
            None => (None, None, None),
        };
        let mut tools = self.tools.tool_set_with_blacklist(config.tool_blacklist);
        let (effect_emitter, effect_inbox) = agent_effect_channel();
        tools.add_group(internal_tools(effect_emitter.clone()))?;
        for extension in environment.extension_tools {
            tools.add_group(extension)?;
        }
        let resumed_adapter =
            ResumedContextAdapter::new(resumed_state, resume_notice, tools.discovery());

        // Every agent gets a transcript for context management; `persists` only
        // decides whether it is written to disk.
        let transcript_id = environment.transcript.id();
        let store = if environment.transcript.persists() {
            match TranscriptStore::<Filesystem>::new(transcript_id, &self.transcript_dir) {
                Ok(store) => store,
                Err(error) => {
                    tracing::error!(
                        name: "transcript_open_failed",
                        agent = %id,
                        kind = %kind.as_str(),
                    );
                    return Err(FsAgentCreateError::Transcript(error));
                }
            }
        } else {
            TranscriptStore::<Filesystem>::in_memory(transcript_id)
        };
        let conversation_history_store = store.clone();

        let conversation_history =
            ConversationHistoryContextAdapter::with_llm_compaction::<Http, Timer>(
                conversation_history_store,
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
                return Err(FsAgentCreateError::LongTerm(error));
            }
        };
        // Factory is the only configured-agent assembly point. BaseAgent sees
        // one generic, immutable adapter set; concrete mode, memory, and skill
        // semantics do not leak into its runtime protocol. ResumedContextAdapter
        // is the boundary that contributes resume context and exposes the pure
        // discovery group implemented alongside the resumed adapter.
        let context_adapters: Vec<Box<dyn ContextAdapter>> = vec![
            Box::new(AgentModeContextAdapter::new(mode_state, effect_emitter)),
            Box::new(resumed_adapter),
            Box::new(conversation_history),
            Box::new(SkillContextAdapter::new(config.skills)),
            Box::new(profile_adapter),
            Box::new(adapter),
        ];
        let base_config = BaseAgentConfig {
            transcript: Box::new(store),
            tools,
            effect_inbox,
            permission_policy: environment.permission_policy,
            agent_instruction: Block::new(BlockKind::AgentInstruction, config.system_prompt),
            inherited_context: environment.inherited_context,
            context_adapters,
            retry_policy: config.retry_policy,
            block_retries: config.tool_block_retries,
        };
        let mut agent = match BaseAgent::<Http, Timer>::build(base_config) {
            Ok(agent) => agent,
            Err(error) => {
                tracing::error!(
                    name: "agent_build_failed",
                    agent = %id,
                    kind = %kind.as_str(),
                );
                return Err(FsAgentCreateError::Agent(error));
            }
        };

        if !goal.as_str().trim().is_empty() {
            if let Err(error) = agent.send_command(AgentCommand::AppendMessage(goal)) {
                tracing::error!(
                    name: "goal_seed_failed",
                    agent = %id,
                    kind = %kind.as_str(),
                );
                return Err(FsAgentCreateError::Goal(error));
            }
        }

        tracing::info!(name: "created", agent = %id, kind = %kind.as_str());
        Ok(agent)
    }

    fn resolve_config(&self, kind: &AgentKind) -> Result<AgentConfig, AgentConfigError> {
        let manifest = agent_catalog::find(kind)
            .ok_or_else(|| AgentConfigError::UnknownKind(kind.as_str().to_owned()))?;
        let runtime = manifest.runtime();
        Ok(AgentConfig::from_manifest(runtime, self.skills.skill_set()))
    }
}
