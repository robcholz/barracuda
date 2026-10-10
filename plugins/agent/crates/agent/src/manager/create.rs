use alloc::{borrow::ToOwned, boxed::Box, rc::Rc, vec::Vec};

use barracuda_agent_context::{Block, BlockKind};
use barracuda_agent_memory::{Transcript, TranscriptStore, TransientTranscript};
use barracuda_agent_permission::PermissionPolicy;
use barracuda_agent_persistence::DurableState;
use barracuda_agent_skill::SkillSetSource;
use barracuda_agent_tool::{BackgroundToolPool, ToolGroup, ToolSetSource};
use barracuda_model_api::RetryPolicy;
use http_client::embedded_nal_async::{Dns, TcpConnect};
use portable_atomic_util::Arc;

use crate::baked;
use crate::config::ApiPurpose;
use crate::context_providers::{
    AgentModeContextProvider, ConversationHistoryContextProvider, ProfileContextProvider,
    ReasoningEffortContextProvider, ResumeContextProvider, SkillContextProvider,
    TodoContextProvider, ToolDiscoveryContextProvider,
};
use crate::engine::{
    agent_effect_channel, AgentEngine, AgentEngineBuildError, AgentEngineConfig, ContextProvider,
};
use crate::tools::{background_tools, internal_tools};
use crate::{Agent, AgentEngineState, AgentKind, ReasoningEffort, ReasoningEffortHandle};

use super::error::AgentCreateError;
use super::{AgentId, AgentManager};

const COMPACTION_TRIGGER_TOKENS: usize = 6000;
const COMPACTION_KEEP_RECENT_TOKENS: usize = 2000;
const COMPACTION_SEGMENT_TOKEN_BUDGET: usize = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceConfig {
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
    state: Option<DurableState<AgentEngineState>>,
}

impl<Tcp, Resolver> AgentManager<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub fn resume_from(
        &self,
        id: AgentId,
        is_root: bool,
        permission_policy: Arc<dyn PermissionPolicy + 'static>,
        reasoning_effort: ReasoningEffort,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(Agent<Tcp, Resolver>, ReasoningEffortHandle), AgentCreateError> {
        let persisted = self.load_persisted_agent(id)?;
        let kind = persisted.get().kind();
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
            },
        )?;
        Ok((agent, reasoning_effort_handle))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        id: AgentId,
        kind: &AgentKind,
        is_root: bool,
        permission_policy: Arc<dyn PermissionPolicy + 'static>,
        reasoning_effort: ReasoningEffort,
        persistence_config: PersistenceConfig,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(Agent<Tcp, Resolver>, ReasoningEffortHandle), AgentCreateError> {
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
            },
        )?;
        if persistence_config == PersistenceConfig::Persistent {
            self.register_new_agent(id, agent.state())?;
        }
        Ok((agent, reasoning_effort_handle))
    }

    fn open_transcript(
        &self,
        id: AgentId,
        _kind: &AgentKind,
        persistence: PersistenceConfig,
    ) -> Result<Box<dyn Transcript>, AgentCreateError> {
        match persistence {
            PersistenceConfig::Persistent => {
                let transcript = self
                    .transcripts
                    .borrow_mut()
                    .entry(id)
                    .or_insert_with(|| {
                        Rc::new(TranscriptStore::empty(
                            self.filesystem.clone(),
                            id.0,
                            &self.transcript_dir,
                        ))
                    })
                    .clone();
                Ok(Box::new(transcript) as Box<dyn Transcript>)
            }
            PersistenceConfig::InMemory => Ok(Box::new(TransientTranscript::new())),
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
    ) -> Result<(Agent<Tcp, Resolver>, ReasoningEffortHandle), AgentCreateError> {
        let span = tracing::info_span!("agent.create");
        let _enter = span.enter();
        let AgentEnvironment {
            transcript,
            is_root,
            permission_policy,
            extension_tools,
            inherited_context,
            reasoning_effort,
            state: recovery_state,
        } = environment;

        let manifest = baked::find(kind).ok_or_else(|| {
            log::error!("unknown Agent kind: {}", kind.as_str());
            tracing::error!(name: "unknown_kind", kind = %kind.as_str());
            AgentCreateError::UnknownKind(kind.as_str().to_owned())
        })?;
        let runtime = manifest.runtime();
        let skill_set = self.skill_registry.skill_set();
        let state =
            recovery_state.unwrap_or_else(|| DurableState::new(AgentEngineState::new(kind)));
        // The per-kind blacklist stays attached to this ToolSet projection so
        // registry refreshes and later local groups follow the same exact-name
        // policy.
        let mut tools = self
            .tool_registry
            .tool_set_with_blacklist(runtime.tool_blacklist());
        let (effect_emitter, effect_inbox) = agent_effect_channel();
        tools.add_group(internal_tools(effect_emitter.clone()))?;
        let background = BackgroundToolPool::new();
        tools.add_group(background_tools(&background))?;
        for extension in extension_tools {
            tools.add_group(extension)?;
        }
        let resume_provider = ResumeContextProvider::new(&state.get());
        let tool_discovery_provider = ToolDiscoveryContextProvider::new(tools.discovery());

        // Only `AgentEngine` holds the transcript (as `dyn Transcript`); context
        // providers read it through the `&dyn Transcript` lent to `prepare`.
        let conversation_history = ConversationHistoryContextProvider::with_llm_compaction(
            Arc::clone(&self.api_manager),
            self.llm_factory.clone(),
            COMPACTION_TRIGGER_TOKENS,
            COMPACTION_KEEP_RECENT_TOKENS,
            COMPACTION_SEGMENT_TOKEN_BUDGET,
        );
        let profile_provider = ProfileContextProvider::new(self.profile_store.clone());
        let provider = match self.long_term.provider(kind.as_str()) {
            Ok(provider) => provider,
            Err(error) => {
                log::error!(
                    "Agent {id} ({}) failed to attach long-term context provider: {error}",
                    kind.as_str()
                );
                tracing::error!(
                    name: "context_provider_attach_failed",
                    agent = %id,
                    provider = "long_term",
                    kind = %kind.as_str(),
                );
                return Err(AgentCreateError::LongTerm(error));
            }
        };
        // AgentManager is the only configured-agent assembly point. AgentEngine sees
        // one generic, immutable provider set; concrete mode, memory, and skill
        // semantics do not leak into its runtime protocol.
        let (reasoning_effort_provider, reasoning_effort_handle) =
            ReasoningEffortContextProvider::new(reasoning_effort);
        let context_providers: Vec<Box<dyn ContextProvider>> = vec![
            Box::new(AgentModeContextProvider::new(effect_emitter)),
            Box::new(reasoning_effort_provider),
            Box::new(TodoContextProvider::new()),
            Box::new(resume_provider),
            Box::new(tool_discovery_provider),
            Box::new(conversation_history),
            Box::new(SkillContextProvider::new(skill_set)),
            Box::new(profile_provider),
            Box::new(provider),
        ];
        let api_purpose = if is_root {
            ApiPurpose::RootAgent
        } else {
            ApiPurpose::SubAgent
        };
        let engine_config = AgentEngineConfig {
            state,
            transcript,
            api_manager: Arc::clone(&self.api_manager),
            api_purpose,
            tools,
            background: background.clone(),
            effect_inbox,
            permission_policy,
            agent_instruction: Block::new(
                BlockKind::AgentInstruction,
                runtime.instructions().trim().to_owned(),
            ),
            inherited_context,
            context_providers,
            retry_policy: RetryPolicy::new(runtime.retries()),
        };
        let engine =
            AgentEngine::build(engine_config, self.llm_factory.create()).map_err(|error| {
                match error {
                    AgentEngineBuildError::InvalidContextProviderId => {
                        AgentCreateError::InvalidContextProviderId
                    }
                    AgentEngineBuildError::DuplicateContextProviderId(id) => {
                        AgentCreateError::DuplicateContextProviderId(id)
                    }
                    AgentEngineBuildError::Tools(error) => AgentCreateError::Tools(error),
                }
            })?;
        let agent = Agent::new(engine, background);

        log::info!("Agent {id} ({}) created", kind.as_str());
        tracing::info!(name: "created", agent = %id, kind = %kind.as_str());
        Ok((agent, reasoning_effort_handle))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use alloc::{boxed::Box, vec::Vec};

    use barracuda_agent_permission::{AllowAll, PermissionPolicy};
    use barracuda_agent_persistence::{DurableState, InstanceId, Persistence};
    use barracuda_agent_tool::ToolRegistry;
    use barracuda_model_api::{ModelApi, ModelApiFactory};
    use barracuda_platform_test::{memory_vfs, NeverStack};
    use futures_lite::future::block_on;
    use http_client::ClientFactory;
    use portable_atomic_util::Arc;

    use crate::{baked, AgentEngineState, AgentId, ReasoningEffort, SharedApiManager};

    use super::{AgentManager, PersistenceConfig};

    static NETWORK: NeverStack = NeverStack;

    #[test]
    fn persisted_agent_resumes_after_manager_restart() {
        block_on(async {
            let filesystem = memory_vfs().await.expect("memory VFS mounts");
            let agent = AgentId::new(1);
            {
                let persistence = Persistence::new(filesystem.clone(), "/agent")
                    .await
                    .expect("persistence opens");
                let state = DurableState::new(AgentEngineState::new(baked::root_kind()));
                persistence
                    .collection::<AgentEngineState>("agents")
                    .expect("Agent collection opens")
                    .register(
                        &InstanceId::new(agent.to_wire()).expect("Agent id is valid"),
                        &state,
                    )
                    .expect("Agent state registers");
                persistence
                    .maybe_persist()
                    .await
                    .expect("Agent state persists");
            }

            let persistence = Arc::new(
                Persistence::new(filesystem.clone(), "/agent")
                    .await
                    .expect("persistence reopens"),
            );
            let tools = Arc::new(
                ToolRegistry::new(Arc::clone(&persistence))
                    .await
                    .expect("tool registry opens"),
            );
            let factory = ModelApiFactory::new(|| {
                ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK))
            });
            let manager = AgentManager::new(
                filesystem,
                tools,
                persistence,
                "/agent".into(),
                Vec::new(),
                SharedApiManager::default(),
                factory,
            )
            .await
            .expect("Agent manager rebuilds");

            manager
                .resume_from(
                    agent,
                    true,
                    Arc::from(Box::new(AllowAll) as Box<dyn PermissionPolicy>),
                    ReasoningEffort::Medium,
                    Vec::new(),
                )
                .expect("persisted Agent resumes without registering its state twice");
        });
    }

    #[test]
    fn fresh_persistent_agent_still_registers_once() {
        block_on(async {
            let filesystem = memory_vfs().await.expect("memory VFS mounts");
            let persistence = Arc::new(
                Persistence::new(filesystem.clone(), "/fresh-agent")
                    .await
                    .expect("persistence opens"),
            );
            let tools = Arc::new(
                ToolRegistry::new(Arc::clone(&persistence))
                    .await
                    .expect("tool registry opens"),
            );
            let factory = ModelApiFactory::new(|| {
                ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK))
            });
            let manager = AgentManager::new(
                filesystem,
                tools,
                Arc::clone(&persistence),
                "/fresh-agent".into(),
                Vec::new(),
                SharedApiManager::default(),
                factory,
            )
            .await
            .expect("Agent manager builds");
            let agent = AgentId::new(1);

            manager
                .create(
                    agent,
                    baked::root_kind(),
                    true,
                    Arc::from(Box::new(AllowAll) as Box<dyn PermissionPolicy>),
                    ReasoningEffort::Medium,
                    PersistenceConfig::Persistent,
                    Vec::new(),
                )
                .expect("fresh persistent Agent creates");
            persistence
                .maybe_persist()
                .await
                .expect("fresh Agent persists");

            assert_eq!(
                persistence
                    .collection::<AgentEngineState>("agents")
                    .expect("Agent collection opens")
                    .list()
                    .await
                    .expect("Agent collection lists"),
                vec![InstanceId::new(agent.to_wire()).expect("Agent id is valid")]
            );
        });
    }
}
