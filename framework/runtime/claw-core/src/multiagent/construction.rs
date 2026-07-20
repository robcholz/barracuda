use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use claw_context::Block;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;

use crate::agent::{
    AgentEnvironment, AgentMode, AgentResume, FsAgentCreateError, FsAgentFactory, TranscriptTarget,
};
use crate::protocol::{AgentId, AgentKind, Message, SessionId, SessionPersistence};

use super::{
    tools, AgentIdAllocator, AgentPlacement, AgentSlots, MultiagentBridge, MultiagentRuntime,
    MultiagentState,
};

impl<Filesystem, Http, Timer> MultiagentRuntime<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    /// Create an empty instance for `session`.
    #[cfg(test)]
    pub(crate) fn new(
        session: SessionId,
        factory: Rc<FsAgentFactory<Filesystem, Http, Timer>>,
        agent_id_allocator: AgentIdAllocator,
        permission_policy: Arc<dyn PermissionPolicy>,
        state: MultiagentState,
    ) -> Self {
        Self::new_with_resume(
            session,
            factory,
            agent_id_allocator,
            permission_policy,
            state,
            AgentMode::Normal,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_resume(
        session: SessionId,
        factory: Rc<FsAgentFactory<Filesystem, Http, Timer>>,
        agent_id_allocator: AgentIdAllocator,
        permission_policy: Arc<dyn PermissionPolicy>,
        state: MultiagentState,
        root_mode: AgentMode,
        root_resume: Option<AgentResume>,
    ) -> Self {
        let multiagent = Arc::new(MultiagentBridge::new(agent_id_allocator.clone()));
        Self {
            session,
            factory,
            permission_policy,
            root_mode,
            root_resume,
            root_deliveries_in_turn: Vec::new(),
            root_background_spawns: BTreeMap::new(),
            agent_id_allocator,
            state,
            slots: AgentSlots::new(),
            timeouts: Default::default(),
            foreground_results: BTreeMap::new(),
            pending_deliveries: Default::default(),
            multiagent,
        }
    }

    pub(super) fn build_agent(
        &mut self,
        id: AgentId,
        kind: &AgentKind,
        goal: Message,
        placement: AgentPlacement,
        inherited_context: Vec<Block<'static>>,
    ) -> Result<(), FsAgentCreateError> {
        let extension_tools = tools::tool_group(id, kind, Arc::clone(&self.multiagent))
            .into_iter()
            .collect();
        let (transcript, initial_mode, resume) = match placement {
            AgentPlacement::Root {
                session,
                persistence,
            } => match persistence {
                SessionPersistence::Persistent => (
                    TranscriptTarget::Persistent(session.0),
                    self.root_mode,
                    self.root_resume.take(),
                ),
                SessionPersistence::Ephemeral => (
                    TranscriptTarget::InMemory(session.0),
                    self.root_mode,
                    self.root_resume.take(),
                ),
            },
            AgentPlacement::Child(child) => {
                (TranscriptTarget::InMemory(child.0), AgentMode::Normal, None)
            }
        };
        let environment = AgentEnvironment::new(
            transcript,
            Arc::clone(&self.permission_policy),
            extension_tools,
            inherited_context,
            initial_mode,
            resume,
        );
        let agent = self.factory.create_agent(id, kind, goal, environment)?;
        self.slots.insert(id, agent);
        Ok(())
    }

    pub(crate) fn root_recovery(&self) -> Option<(AgentMode, Vec<String>)> {
        let root = self.state.root()?;
        let agent = self.slots.available_agent(root)?;
        Some((agent.mode(), agent.loaded_tool_groups()))
    }

    pub(crate) fn root_resume_pending(&self) -> bool {
        self.state
            .root()
            .and_then(|root| self.slots.available_agent(root))
            .is_some_and(|agent| agent.resume_pending())
    }

    pub(crate) fn active_root_background_spawns(&self) -> Vec<crate::protocol::TrackedToolCall> {
        self.root_background_spawns.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::{Arc, RwLock};

    use claw_interface::{ImmediateTimer, MemFs, RealHttp};
    use claw_permission::AllowAll;
    use claw_tool::ToolRegistry;

    use crate::agent::FsAgentFactory;
    use crate::config::{catalog as agent_catalog, ClawApiManager};
    use crate::protocol::{AgentId, AgentKind, Message, SessionId, SessionPersistence};

    use super::super::{AgentIdAllocator, AgentPlacement, MultiagentRuntime, MultiagentState};

    type TestRuntime = MultiagentRuntime<MemFs, RealHttp, ImmediateTimer>;

    #[test]
    fn baked_blacklist_projects_worker_plan_and_profile_tools() {
        MemFs::new();
        let session = SessionId::new(1);
        let factory = Rc::new(
            FsAgentFactory::new(
                Arc::new(ToolRegistry::new()),
                "/plan-mode-tools-test".to_owned(),
                Vec::new(),
                Arc::new(RwLock::new(ClawApiManager::new())),
            )
            .expect("test factory builds"),
        );
        let mut runtime = TestRuntime::new(
            session,
            factory,
            AgentIdAllocator::new(),
            Arc::new(AllowAll),
            MultiagentState::default(),
        );
        let root = AgentId::new(1);
        let child = AgentId::new(2);

        runtime
            .build_agent(
                root,
                agent_catalog::root_kind(),
                Message::text("root"),
                AgentPlacement::Root {
                    session,
                    persistence: SessionPersistence::Ephemeral,
                },
                Vec::new(),
            )
            .expect("root builds");
        runtime
            .build_agent(
                child,
                &AgentKind::from_static("worker"),
                Message::text("child"),
                AgentPlacement::Child(child),
                Vec::new(),
            )
            .expect("child builds");

        for name in ["plan_enter", "plan_exit", "plan_clarify"] {
            assert!(runtime
                .slots
                .available_agent_mut(root)
                .expect("root is available")
                .exposes_tool_for_test(name));
            assert!(!runtime
                .slots
                .available_agent_mut(child)
                .expect("child is available")
                .exposes_tool_for_test(name));
        }

        for id in [root, child] {
            assert!(runtime
                .slots
                .available_agent_mut(id)
                .expect("agent is available")
                .exposes_tool_for_test("profile_read"));
        }
        for name in ["profile_replace", "profile_clear"] {
            assert!(runtime
                .slots
                .available_agent_mut(root)
                .expect("root is available")
                .exposes_tool_for_test(name));
            assert!(!runtime
                .slots
                .available_agent_mut(child)
                .expect("child is available")
                .exposes_tool_for_test(name));
        }
    }
}
