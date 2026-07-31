use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::{LongTermInitError, LongTermMemory};

use crate::agent::context_providers::LongTermMemoryContextProvider;
use crate::config::SharedApiManager;

use super::layout::join_storage_path;

const GLOBAL_LONG_TERM_DIR: &str = "g";
const AGENT_LONG_TERM_DIR: &str = "a";

type BuildProvider<F> =
    dyn Fn(LongTermMemory<F>, LongTermMemory<F>) -> LongTermMemoryContextProvider<F>;

pub(super) struct LongTermDeps<F: ClawFs + 'static> {
    global: LongTermMemory<F>,
    agent_stores: AgentMemoryStores<F>,
    build_provider: Arc<BuildProvider<F>>,
}

struct AgentMemoryStores<F: ClawFs + 'static> {
    filesystem: Arc<F>,
    root_dir: String,
    by_kind: Mutex<BTreeMap<String, LongTermMemory<F>>>,
}

impl<F: ClawFs + 'static> AgentMemoryStores<F> {
    fn new(filesystem: Arc<F>, root_dir: String) -> Self {
        Self {
            filesystem,
            root_dir,
            by_kind: Mutex::new(BTreeMap::new()),
        }
    }

    fn get(&self, kind: &str) -> Result<LongTermMemory<F>, LongTermInitError> {
        let mut stores = self
            .by_kind
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(store) = stores.get(kind) {
            return Ok(store.clone());
        }

        let dir = join_storage_path(&self.root_dir, kind);
        let store = LongTermMemoryContextProvider::<F>::open_agent_store(
            Arc::clone(&self.filesystem),
            &dir,
        )?;
        stores.insert(kind.to_owned(), store.clone());
        Ok(store)
    }
}

impl<F: ClawFs + 'static> LongTermDeps<F> {
    pub(super) fn from_root<H, Timer>(
        filesystem: Arc<F>,
        long_term_dir: &str,
        api_manager: SharedApiManager,
    ) -> Result<Self, LongTermInitError>
    where
        H: ClawHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    {
        let global_dir = join_storage_path(long_term_dir, GLOBAL_LONG_TERM_DIR);
        let agent_root_dir = join_storage_path(long_term_dir, AGENT_LONG_TERM_DIR);
        Ok(Self {
            global: LongTermMemoryContextProvider::<F>::open_global_store(
                Arc::clone(&filesystem),
                &global_dir,
            )?,
            agent_stores: AgentMemoryStores::new(filesystem, agent_root_dir),
            build_provider: LongTermMemoryContextProvider::<F>::llm_builder::<H, Timer>(
                api_manager,
            ),
        })
    }

    pub(super) fn provider(
        &self,
        kind: &str,
    ) -> Result<LongTermMemoryContextProvider<F>, LongTermInitError> {
        let agent = self.agent_stores.get(kind)?;
        Ok((self.build_provider)(agent, self.global.clone()))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use claw_interface::MemFs;
    use claw_memory::{MemoryDraft, StoreOutcome};

    use super::AgentMemoryStores;

    #[test]
    fn same_kind_reuses_one_live_store_owner() {
        let stores =
            AgentMemoryStores::<MemFs>::new(Arc::new(MemFs::new()), "/memory/a".to_owned());
        let first = stores.get("conversation").expect("first store opens");
        let second = stores.get("conversation").expect("second store opens");

        assert!(matches!(
            first.store(MemoryDraft::new("fact from first agent")),
            StoreOutcome::Created(_)
        ));
        assert_eq!(second.list().len(), 1);
    }
}
