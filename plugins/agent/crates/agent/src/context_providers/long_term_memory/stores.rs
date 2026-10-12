use alloc::{string::String, vec::Vec};

use barracuda_agent_memory::{
    LongTermError, LongTermInitError, LongTermMemory, MemoryDraft, MemoryId, MemoryItem,
    MemoryPatch, StoreOutcome,
};
use barracuda_vfs::ScopedVfs;

use super::tier::classify_tier;
use super::{MemorySnapshot, MemoryTier};

/// Id prefix for the shared global store.
pub(super) const GLOBAL_ID_PREFIX: &str = "g-";
/// Id prefix for the per-agent store.
pub(super) const AGENT_ID_PREFIX: &str = "a-";

/// Build a global long-term store under `dir` (minting `g-` ids).
///
/// # Errors
///
/// Propagates [`LongTermInitError`] when the journal exists but is unreadable.
pub(super) async fn global_store(
    filesystem: ScopedVfs,
    dir: &str,
) -> Result<LongTermMemory, LongTermInitError> {
    LongTermMemory::new(filesystem, dir, GLOBAL_ID_PREFIX).await
}

/// Build a per-agent long-term store under `dir` (minting `a-` ids).
///
/// # Errors
///
/// Propagates [`LongTermInitError`] when the journal exists but is unreadable.
pub(super) async fn agent_store(
    filesystem: ScopedVfs,
    dir: &str,
) -> Result<LongTermMemory, LongTermInitError> {
    LongTermMemory::new(filesystem, dir, AGENT_ID_PREFIX).await
}

/// The two stores, shared (by cheap clone) between the provider and every memory
/// tool handler.
pub(super) struct MemoryStores {
    pub(super) global: LongTermMemory,
    pub(super) agent: LongTermMemory,
}

impl Clone for MemoryStores {
    fn clone(&self) -> Self {
        Self {
            global: self.global.clone(),
            agent: self.agent.clone(),
        }
    }
}

impl MemoryStores {
    /// Store a draft in the tier determined by its tags, unless either tier
    /// already holds the same fact.
    pub(crate) async fn store(&self, draft: MemoryDraft) -> StoreOutcome {
        let (target, other) = match classify_tier(&draft) {
            MemoryTier::Global => (&self.global, &self.agent),
            MemoryTier::Agent => (&self.agent, &self.global),
        };
        if let Some(existing) = other.find_duplicate(&draft.content) {
            return StoreOutcome::Duplicate(existing);
        }
        target.store(draft).await
    }

    /// Recall across both stores (global first), capped at `limit` total.
    pub(crate) fn recall(
        &self,
        labels: &[String],
        query: Option<&str>,
        limit: usize,
    ) -> Vec<MemoryItem> {
        let mut hits = self.global.recall(labels, query, limit);
        hits.extend(self.agent.recall(labels, query, limit));
        hits.truncate(limit);
        hits
    }

    /// All facts across both stores (global first).
    pub(crate) fn list(&self) -> Vec<MemoryItem> {
        let mut items = self.global.list();
        items.extend(self.agent.list());
        items
    }

    pub(crate) fn snapshot(&self) -> Vec<MemorySnapshot> {
        self.list()
            .into_iter()
            .map(|item| MemorySnapshot {
                id: item.id,
                content: item.content,
                tags: item.tags,
            })
            .collect()
    }

    /// Apply a patch to the item with `id`, routing by its prefix.
    pub(crate) async fn update(
        &self,
        id: &MemoryId,
        patch: MemoryPatch,
    ) -> Result<MemoryItem, LongTermError> {
        self.store_for(id).update(id, patch).await
    }

    /// Forget the item with `id`, routing by its prefix.
    pub(crate) async fn forget(&self, id: &MemoryId) -> Result<(), LongTermError> {
        self.store_for(id).forget(id).await
    }

    fn store_for(&self, id: &MemoryId) -> &LongTermMemory {
        if id.as_str().starts_with(GLOBAL_ID_PREFIX) {
            &self.global
        } else {
            &self.agent
        }
    }
}
