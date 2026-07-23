use std::collections::{BTreeMap, BTreeSet, VecDeque};

use claw_api::ToolCall;

use crate::agent::ToolCallId;
use crate::agent::AgentId;

use super::model::SubagentStatus;

#[derive(Clone)]
pub(crate) struct NodeMeta {
    parent: Option<AgentId>,
}

impl NodeMeta {
    pub(crate) fn new(parent: Option<AgentId>) -> Self {
        Self { parent }
    }

    pub(crate) fn parent(&self) -> Option<AgentId> {
        self.parent
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParkedApproval {
    pub(crate) tool_call_id: ToolCallId,
    pub(crate) tool_call: ToolCall,
    pub(crate) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MultiagentWork {
    None,
    Root,
    Background,
}

/// The complete process-local graph and scheduler state for one session.
///
/// Topology, ready work, and approvals are one owner so mutations such as
/// subtree removal cannot update only half of the runtime state.
#[derive(Default)]
pub(crate) struct MultiagentState {
    nodes: BTreeMap<AgentId, NodeMeta>,
    ready: VecDeque<AgentId>,
    approvals: VecDeque<(AgentId, ParkedApproval)>,
}

impl MultiagentState {
    #[cfg(test)]
    pub(crate) fn restored(
        nodes: BTreeMap<AgentId, NodeMeta>,
        ready: VecDeque<AgentId>,
        approvals: VecDeque<(AgentId, ParkedApproval)>,
    ) -> Self {
        Self {
            nodes,
            ready,
            approvals,
        }
    }

    pub(crate) fn root(&self) -> Option<AgentId> {
        let mut roots = self
            .nodes
            .iter()
            .filter_map(|(&id, meta)| meta.parent.is_none().then_some(id));
        let root = roots.next()?;
        roots.next().is_none().then_some(root)
    }

    pub(crate) fn is_root(&self, id: AgentId) -> bool {
        self.root() == Some(id)
    }

    pub(crate) fn contains(&self, id: AgentId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub(crate) fn parent(&self, id: AgentId) -> Option<Option<AgentId>> {
        self.nodes.get(&id).map(NodeMeta::parent)
    }

    pub(crate) fn node(&self, id: AgentId) -> Option<&NodeMeta> {
        self.nodes.get(&id)
    }

    #[cfg(test)]
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub(crate) fn insert_root(&mut self, id: AgentId) -> bool {
        if self.root().is_some() || self.nodes.contains_key(&id) {
            return false;
        }
        self.nodes.insert(id, NodeMeta::new(None));
        true
    }

    #[must_use]
    pub(crate) fn insert_child(&mut self, parent: AgentId, id: AgentId) -> bool {
        if !self.nodes.contains_key(&parent) || self.nodes.contains_key(&id) {
            return false;
        }
        self.nodes.insert(id, NodeMeta::new(Some(parent)));
        true
    }

    pub(crate) fn has_children(&self, parent: AgentId) -> bool {
        self.nodes.values().any(|meta| meta.parent == Some(parent))
    }

    pub(crate) fn root_children(&self) -> Vec<AgentId> {
        let Some(root) = self.root() else {
            return Vec::new();
        };
        self.nodes
            .iter()
            .filter_map(|(&id, meta)| (meta.parent == Some(root)).then_some(id))
            .collect()
    }

    pub(crate) fn is_strict_descendant(&self, ancestor: AgentId, node: AgentId) -> bool {
        if ancestor == node {
            return false;
        }
        let mut seen = BTreeSet::new();
        let mut current = self.nodes.get(&node).and_then(|meta| meta.parent);
        while let Some(parent) = current {
            if parent == ancestor {
                return true;
            }
            if !seen.insert(parent) {
                return false;
            }
            current = self.nodes.get(&parent).and_then(|meta| meta.parent);
        }
        false
    }

    pub(crate) fn depth(&self, id: AgentId) -> Option<u16> {
        let mut current = id;
        let mut depth = 0_u16;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current) {
                return None;
            }
            let meta = self.nodes.get(&current)?;
            let Some(parent) = meta.parent else {
                return Some(depth);
            };
            depth = depth.checked_add(1)?;
            current = parent;
        }
    }

    pub(crate) fn subtree_ids(&self, root: AgentId) -> Vec<AgentId> {
        let mut out = Vec::new();
        let mut frontier = VecDeque::from([root]);
        let mut visited = BTreeSet::new();
        while let Some(current) = frontier.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            out.push(current);
            for (&id, meta) in &self.nodes {
                if meta.parent == Some(current) {
                    frontier.push_back(id);
                }
            }
        }
        out
    }

    pub(crate) fn remove_agents(&mut self, agents: &[AgentId]) {
        for id in agents {
            self.nodes.remove(id);
        }
        self.ready.retain(|queued| !agents.contains(queued));
        self.approvals.retain(|(agent, _)| !agents.contains(agent));
    }

    pub(crate) fn enqueue(&mut self, id: AgentId) {
        if !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }

    pub(crate) fn pop_ready(&mut self) -> Option<AgentId> {
        self.ready.pop_front()
    }

    pub(crate) fn clear_turn_work(&mut self) {
        self.ready.clear();
        self.approvals.clear();
    }

    pub(crate) fn work(&self, root_running: bool, background_running: bool) -> MultiagentWork {
        let root = self.root();
        let root_ready = root.is_some_and(|root| self.ready.contains(&root));
        if root_ready || root_running || self.has_pending_approval() {
            return MultiagentWork::Root;
        }
        let background_ready = self.ready.iter().any(|id| Some(*id) != root);
        if background_ready || background_running {
            MultiagentWork::Background
        } else {
            MultiagentWork::None
        }
    }

    pub(crate) fn agent_status(&self, id: AgentId, running: bool) -> SubagentStatus {
        if self.is_awaiting_approval(id) {
            SubagentStatus::AwaitingApproval
        } else if running {
            SubagentStatus::Running
        } else if self.ready.contains(&id) {
            SubagentStatus::Ready
        } else {
            SubagentStatus::Idle
        }
    }

    pub(crate) fn has_pending_approval(&self) -> bool {
        !self.approvals.is_empty()
    }

    pub(crate) fn active_approval(&self) -> Option<(AgentId, &ParkedApproval)> {
        let (agent, pending) = self.approvals.front()?;
        Some((*agent, pending))
    }

    pub(crate) fn park_approval(
        &mut self,
        agent: AgentId,
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    ) {
        let replacement = ParkedApproval {
            tool_call_id,
            tool_call,
            reason,
        };
        if let Some((_, pending)) = self
            .approvals
            .iter_mut()
            .find(|(queued_agent, _)| *queued_agent == agent)
        {
            *pending = replacement;
        } else {
            self.approvals.push_back((agent, replacement));
        }
    }

    pub(crate) fn is_awaiting_approval(&self, agent: AgentId) -> bool {
        self.approvals
            .iter()
            .any(|(queued_agent, _)| *queued_agent == agent)
    }

    pub(crate) fn remove_approval(&mut self, agent: AgentId) -> bool {
        let Some(position) = self
            .approvals
            .iter()
            .position(|(queued_agent, _)| *queued_agent == agent)
        else {
            return false;
        };
        self.approvals.remove(position).is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use claw_api::ToolCall;

    use crate::agent::ToolCallId;
    use crate::agent::AgentId;

    use super::{MultiagentState, MultiagentWork, NodeMeta};

    #[test]
    fn topology_owns_descendant_and_subtree_rules() {
        let root = AgentId(1);
        let child = AgentId(2);
        let grandchild = AgentId(3);
        let mut state = MultiagentState::default();
        assert!(state.insert_root(root));
        assert!(state.insert_child(root, child));
        assert!(state.insert_child(child, grandchild));

        assert!(state.is_strict_descendant(root, grandchild));
        assert_eq!(state.depth(grandchild), Some(2));
        assert_eq!(state.subtree_ids(child), vec![child, grandchild]);
    }

    #[test]
    fn malformed_topology_walks_terminate() {
        let first = AgentId(1);
        let second = AgentId(2);
        let nodes = BTreeMap::from([
            (first, NodeMeta::new(Some(second))),
            (second, NodeMeta::new(Some(first))),
        ]);
        let state = MultiagentState::restored(nodes, Default::default(), Default::default());

        assert_eq!(state.subtree_ids(first), vec![first, second]);
        assert_eq!(state.depth(first), None);
    }

    #[test]
    fn work_prioritizes_root_over_background_agents() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut state = MultiagentState::default();
        assert!(state.insert_root(root));
        assert!(state.insert_child(root, child));

        assert_eq!(state.work(false, false), MultiagentWork::None);
        state.enqueue(child);
        assert_eq!(state.work(false, false), MultiagentWork::Background);
        state.enqueue(root);
        assert_eq!(state.work(false, false), MultiagentWork::Root);
    }

    #[test]
    fn approvals_are_resolved_in_queue_order() {
        let first = AgentId(1);
        let second = AgentId(2);
        let mut state = MultiagentState::default();
        state.park_approval(
            first,
            ToolCallId::new(0),
            ToolCall::default(),
            "first".to_owned(),
        );
        state.park_approval(
            second,
            ToolCallId::new(1),
            ToolCall::default(),
            "second".to_owned(),
        );

        let (agent, approval) = state.active_approval().expect("first approval is active");
        assert_eq!(agent, first);
        assert_eq!(approval.tool_call_id, ToolCallId::new(0));
        assert_eq!(approval.tool_call, ToolCall::default());
        assert_eq!(approval.reason, "first");
        assert!(state.has_pending_approval());
        assert!(state.remove_approval(first));
        let (agent, approval) = state.active_approval().expect("second approval is active");
        assert_eq!(agent, second);
        assert_eq!(approval.tool_call_id, ToolCallId::new(1));
        assert_eq!(approval.reason, "second");
    }
}
