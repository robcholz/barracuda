use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::agent::{AgentId, AgentKind};

use super::model::{SubagentSnapshot, SubagentStatus, SubagentTimeout};

#[derive(Clone)]
pub(crate) struct NodeMeta {
    parent: Option<AgentId>,
    kind: AgentKind,
    name: Option<String>,
    timeout: Option<SubagentTimeout>,
    status: SubagentStatus,
}

impl NodeMeta {
    fn root(kind: AgentKind) -> Self {
        Self {
            parent: None,
            kind,
            name: None,
            timeout: None,
            status: SubagentStatus::Idle,
        }
    }

    fn child(
        parent: AgentId,
        kind: AgentKind,
        name: Option<String>,
        timeout: SubagentTimeout,
    ) -> Self {
        Self {
            parent: Some(parent),
            kind,
            name,
            timeout: Some(timeout),
            status: SubagentStatus::Ready,
        }
    }

    pub(crate) fn parent(&self) -> Option<AgentId> {
        self.parent
    }

    pub(crate) fn kind(&self) -> &AgentKind {
        &self.kind
    }

    pub(crate) fn timeout(&self) -> Option<SubagentTimeout> {
        self.timeout
    }

    fn snapshot(&self, id: AgentId, depth: u16) -> SubagentSnapshot {
        SubagentSnapshot::new(
            id,
            self.kind.clone(),
            self.name.clone(),
            self.parent,
            depth,
            self.status,
        )
    }
}

/// The process-local topology and lifecycle state for one Session.
///
/// Live Agents and timers stay outside this type. Every status transition is
/// nevertheless recorded here before the corresponding physical effect is
/// issued, making late Agent and timeout events recognizable as stale.
#[derive(Default)]
pub(crate) struct MultiagentState {
    nodes: BTreeMap<AgentId, NodeMeta>,
}

impl MultiagentState {
    pub(crate) fn root(&self) -> Option<AgentId> {
        let mut roots = self
            .nodes
            .iter()
            .filter_map(|(&id, meta)| meta.parent.is_none().then_some(id));
        let root = roots.next()?;
        roots.next().is_none().then_some(root)
    }

    pub(crate) fn contains(&self, id: AgentId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub(crate) fn node(&self, id: AgentId) -> Option<&NodeMeta> {
        self.nodes.get(&id)
    }

    pub(crate) fn status(&self, id: AgentId) -> Option<SubagentStatus> {
        self.nodes.get(&id).map(|node| node.status)
    }

    pub(crate) fn set_status(&mut self, id: AgentId, status: SubagentStatus) -> bool {
        let Some(node) = self.nodes.get_mut(&id) else {
            return false;
        };
        node.status = status;
        true
    }

    pub(crate) fn agent_ids(&self) -> impl Iterator<Item = AgentId> + '_ {
        self.nodes.keys().copied()
    }

    #[must_use]
    pub(crate) fn insert_root(&mut self, id: AgentId, kind: AgentKind) -> bool {
        if self.root().is_some() || self.nodes.contains_key(&id) {
            return false;
        }
        self.nodes.insert(id, NodeMeta::root(kind));
        true
    }

    #[must_use]
    pub(crate) fn insert_child(
        &mut self,
        parent: AgentId,
        id: AgentId,
        kind: AgentKind,
        name: Option<String>,
        timeout: SubagentTimeout,
    ) -> bool {
        if self.nodes.contains_key(&id)
            || !self.nodes.get(&parent).is_some_and(|node| {
                !matches!(
                    node.status,
                    SubagentStatus::Reaping | SubagentStatus::CompletedPendingDelivery
                )
            })
        {
            return false;
        }
        self.nodes
            .insert(id, NodeMeta::child(parent, kind, name, timeout));
        true
    }

    pub(crate) fn parent(&self, id: AgentId) -> Option<Option<AgentId>> {
        self.nodes.get(&id).map(NodeMeta::parent)
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
        let mut current = self.nodes.get(&node).and_then(NodeMeta::parent);
        while let Some(parent) = current {
            if parent == ancestor {
                return true;
            }
            if !seen.insert(parent) {
                return false;
            }
            current = self.nodes.get(&parent).and_then(NodeMeta::parent);
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
        if !self.nodes.contains_key(&root) {
            return Vec::new();
        }
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
    }

    pub(crate) fn snapshots(&self) -> Vec<SubagentSnapshot> {
        self.nodes
            .iter()
            .filter_map(|(&id, node)| self.depth(id).map(|depth| node.snapshot(id, depth)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeout() -> SubagentTimeout {
        SubagentTimeout::from_millis(60_000).expect("test timeout is non-zero")
    }

    #[test]
    fn topology_owns_depth_descendant_and_subtree_rules() {
        let root = AgentId(1);
        let child = AgentId(2);
        let grandchild = AgentId(3);
        let mut state = MultiagentState::default();
        assert!(state.insert_root(root, AgentKind::from_static("conversation")));
        assert!(state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(state.insert_child(
            child,
            grandchild,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));

        assert!(state.is_strict_descendant(root, grandchild));
        assert_eq!(state.depth(grandchild), Some(2));
        assert_eq!(state.subtree_ids(child), vec![child, grandchild]);
    }

    #[test]
    fn duplicate_roots_and_orphan_children_are_rejected() {
        let root = AgentId(1);
        let mut state = MultiagentState::default();
        assert!(state.insert_root(root, AgentKind::from_static("conversation")));
        assert!(!state.insert_root(AgentId(2), AgentKind::from_static("conversation")));
        assert!(!state.insert_child(
            AgentId(99),
            AgentId(3),
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
    }
}
