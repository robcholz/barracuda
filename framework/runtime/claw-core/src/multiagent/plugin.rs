use core::task::{Context, Poll};
use std::sync::Arc;

use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};

use super::model::MultiagentSnapshot;
use super::state::MultiagentState;
use super::tool_port::{MultiagentAction, MultiagentBridge};

/// Optional multiagent domain component attached to one Session.
///
/// It owns graph state and the tool bridge, but never owns live Agents or
/// physical scheduler state. SessionActor polls commands from this component
/// and performs the requested Agent operations.
pub(crate) struct Multiagent {
    state: MultiagentState,
    bridge: Arc<MultiagentBridge>,
}

impl Multiagent {
    pub(crate) fn new() -> Self {
        Self {
            state: MultiagentState::default(),
            bridge: Arc::new(MultiagentBridge::new()),
        }
    }

    /// Build the caller-bound multiagent tools allowed by `kind`.
    pub(crate) fn tool_group(&self, caller: AgentId, kind: &AgentKind) -> Option<ToolGroup> {
        super::tools::tool_group(caller, kind, Arc::clone(&self.bridge))
    }

    /// Poll one semantic command emitted by an Agent tool.
    pub(crate) fn poll_command(
        &self,
        context: &mut Context<'_>,
    ) -> Poll<(AgentId, MultiagentAction)> {
        self.bridge
            .poll_command(context)
            .map(|command| command.into_parts())
    }

    #[must_use]
    pub(crate) fn insert_root(&mut self, id: AgentId) -> bool {
        self.state.insert_root(id)
    }

    #[must_use]
    pub(crate) fn insert_child(&mut self, parent: AgentId, id: AgentId) -> bool {
        self.state.insert_child(parent, id)
    }

    pub(crate) fn contains(&self, id: AgentId) -> bool {
        self.state.contains(id)
    }

    /// Resolve a caller-authorized subtree operation without exposing graph
    /// traversal rules to SessionActor.
    pub(crate) fn controlled_subtree(
        &self,
        requester: AgentId,
        target: AgentId,
    ) -> Option<Vec<AgentId>> {
        self.state
            .is_strict_descendant(requester, target)
            .then(|| self.state.subtree_ids(target))
    }

    pub(crate) fn remove_agents(&mut self, agents: &[AgentId]) {
        self.state.remove_agents(agents);
    }

    /// Publish the slot-derived read model consumed by list/watch tools.
    pub(crate) fn publish_snapshot(&self, snapshot: MultiagentSnapshot) {
        self.bridge.publish_snapshot(snapshot);
    }

    pub(crate) fn clear(&mut self) {
        self.state = MultiagentState::default();
        self.bridge.clear();
        self.bridge.publish_snapshot(MultiagentSnapshot::default());
    }
}

impl Default for Multiagent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future;

    use super::Multiagent;
    use crate::agent::AgentId;
    use crate::multiagent::MultiagentAction;

    #[test]
    fn component_owns_tool_commands_and_graph_policy() {
        let root = AgentId(1);
        let child = AgentId(2);
        let grandchild = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent
            .tool_group(root, crate::agent::baked::root_kind())
            .is_some());
        assert!(multiagent.insert_root(root));
        assert!(multiagent.insert_child(root, child));
        assert!(multiagent.insert_child(child, grandchild));

        multiagent.bridge.delete(root, child);
        let (requester, action) =
            future::block_on(future::poll_fn(|context| multiagent.poll_command(context)));

        assert_eq!(requester, root);
        assert!(matches!(
            action,
            MultiagentAction::Delete { target } if target == child
        ));
        assert_eq!(
            multiagent.controlled_subtree(root, child),
            Some(vec![child, grandchild])
        );
        assert_eq!(multiagent.controlled_subtree(child, root), None);
    }
}
