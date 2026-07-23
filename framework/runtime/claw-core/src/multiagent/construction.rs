use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;

use crate::agent::{AgentCreateError, AgentManager, PersistenceConfig};
use crate::config::ReasoningEffort;
use crate::protocol::{AgentId, AgentKind, Message, ToolCall};

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
    /// Create an empty runtime.
    #[cfg(test)]
    pub(crate) fn new(
        manager: Rc<AgentManager<Filesystem, Http, Timer>>,
        id_allocator: AgentIdAllocator,
        permission_policy: Arc<dyn PermissionPolicy>,
        state: MultiagentState,
    ) -> Self {
        Self::new_with_root(
            manager,
            id_allocator,
            permission_policy,
            ReasoningEffort::default(),
            state,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_root(
        manager: Rc<AgentManager<Filesystem, Http, Timer>>,
        id_allocator: AgentIdAllocator,
        permission_policy: Arc<dyn PermissionPolicy>,
        reasoning_effort: ReasoningEffort,
        state: MultiagentState,
        restored_root: Option<AgentId>,
    ) -> Self {
        let multiagent = Arc::new(MultiagentBridge::new(id_allocator.clone()));
        Self {
            manager,
            permission_policy,
            reasoning_effort,
            restored_root,
            root_deliveries_in_turn: Vec::new(),
            root_background_spawns: BTreeMap::new(),
            id_allocator,
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
    ) -> Result<AgentKind, AgentCreateError> {
        let extension_tools = tools::tool_group(id, kind, Arc::clone(&self.multiagent))
            .into_iter()
            .collect();
        let (agent, reasoning_effort) = match placement {
            AgentPlacement::FreshRoot(persistence) => self.manager.create(
                id,
                kind,
                true,
                Arc::clone(&self.permission_policy),
                self.reasoning_effort,
                persistence,
                extension_tools,
            )?,
            AgentPlacement::RestoredRoot => self.manager.resume_from(
                id,
                true,
                Arc::clone(&self.permission_policy),
                self.reasoning_effort,
                extension_tools,
                None,
            )?,
            AgentPlacement::Child => self.manager.create(
                id,
                kind,
                false,
                Arc::clone(&self.permission_policy),
                self.reasoning_effort,
                PersistenceConfig::InMemory,
                extension_tools,
            )?,
        };
        let actual_kind = agent.kind().clone();
        let has_goal = !goal.as_str().trim().is_empty();
        self.slots.insert(id, agent, reasoning_effort);
        if has_goal {
            let queued = self.slots.queue_message(id, goal);
            debug_assert!(queued, "a newly inserted agent has a live slot");
        }
        Ok(actual_kind)
    }

    pub(crate) fn root_id(&self) -> Option<AgentId> {
        self.state.root()
    }

    pub(crate) fn active_root_background_spawns(&self) -> Vec<ToolCall> {
        self.root_background_spawns.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use claw_interface::{ImmediateTimer, MemFs, RealHttp};
    use claw_permission::AllowAll;
    use claw_persistence::Persistence;
    use claw_tool::ToolRegistry;

    use crate::agent::{AgentManager, PersistenceConfig};
    use crate::config::SharedApiManager;
    use crate::protocol::Message;

    use super::super::{AgentIdAllocator, MultiagentRuntime, MultiagentState};

    type TestRuntime = MultiagentRuntime<MemFs, RealHttp, ImmediateTimer>;

    #[test]
    fn durable_root_is_restored_by_identity() {
        MemFs::new();
        let persistence = Arc::new(
            Persistence::<MemFs>::new("/agent-state-restore-test/state")
                .expect("test persistence builds"),
        );
        let manager = Rc::new(
            AgentManager::new(
                Arc::new(ToolRegistry::new()),
                Arc::clone(&persistence),
                "/agent-state-restore-test/memory".to_owned(),
                Vec::new(),
                SharedApiManager::default(),
            )
            .expect("test manager builds"),
        );
        let mut runtime = TestRuntime::new(
            Rc::clone(&manager),
            AgentIdAllocator::new(),
            Arc::new(AllowAll),
            MultiagentState::default(),
        );
        runtime
            .deliver(Message::text("fresh root"), PersistenceConfig::Persistent)
            .expect("persistent root builds");
        let root = runtime.root_id().expect("root was inserted");
        persistence
            .maybe_persist()
            .expect("persistent root state is flushed");
        drop(runtime);

        let mut restored = TestRuntime::new_with_root(
            manager,
            AgentIdAllocator::new(),
            Arc::new(AllowAll),
            Default::default(),
            MultiagentState::default(),
            Some(root),
        );
        restored
            .deliver(
                Message::text("restored root"),
                PersistenceConfig::Persistent,
            )
            .expect("persistent root restores");

        assert_eq!(restored.root_id(), Some(root));
    }
}
