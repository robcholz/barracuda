//! In-memory index over the persisted session collection.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

use claw_persistence::DurableState;

use crate::orchestrator::IdAllocators;
use crate::protocol::{SessionId, SessionPersistence};

struct Registry {
    persistent_sessions: BTreeSet<SessionId>,
    ephemeral_sessions: BTreeSet<SessionId>,
}

pub(crate) struct SessionStore {
    ids: DurableState<IdAllocators>,
    registry: Mutex<Registry>,
}

impl SessionStore {
    pub(crate) fn new(
        ids: DurableState<IdAllocators>,
        persistent_sessions: impl IntoIterator<Item = SessionId>,
    ) -> Self {
        let persistent_sessions = persistent_sessions.into_iter().collect::<BTreeSet<_>>();
        let discovered_next = persistent_sessions
            .last()
            .map(|session| session.0.saturating_add(1))
            .unwrap_or(1);
        ids.get_mut()
            .ensure_next_session(SessionId::new(discovered_next));
        Self {
            ids,
            registry: Mutex::new(Registry {
                persistent_sessions,
                ephemeral_sessions: BTreeSet::new(),
            }),
        }
    }

    fn lock_registry(&self) -> MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserve the next id. The engine is the sole writer and publishes it only
    /// after the session's state has been constructed successfully.
    pub(crate) fn allocate(&self) -> SessionId {
        self.ids.get_mut().next_session()
    }

    pub(crate) fn publish(&self, id: SessionId, persistence: SessionPersistence) {
        let mut registry = self.lock_registry();
        match persistence {
            SessionPersistence::Persistent => {
                registry.persistent_sessions.insert(id);
            }
            SessionPersistence::Ephemeral => {
                registry.ephemeral_sessions.insert(id);
            }
        }
    }

    pub(crate) fn list(&self) -> Vec<SessionId> {
        let registry = self.lock_registry();
        registry
            .persistent_sessions
            .union(&registry.ephemeral_sessions)
            .copied()
            .collect()
    }

    pub(crate) fn delete(&self, session_id: SessionId) -> bool {
        let mut registry = self.lock_registry();
        registry.persistent_sessions.remove(&session_id)
            || registry.ephemeral_sessions.remove(&session_id)
    }

    pub(crate) fn contains(&self, session_id: SessionId) -> bool {
        self.persistence(session_id).is_some()
    }

    pub(crate) fn persistence(&self, session_id: SessionId) -> Option<SessionPersistence> {
        let registry = self.lock_registry();
        if registry.persistent_sessions.contains(&session_id) {
            Some(SessionPersistence::Persistent)
        } else if registry.ephemeral_sessions.contains(&session_id) {
            Some(SessionPersistence::Ephemeral)
        } else {
            None
        }
    }
}
