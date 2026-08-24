use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc};
use core::cell::RefCell;

use barracuda_agent_memory::{LongTermInitError, LongTermMemory};
use barracuda_fs::FileSystem;
use barracuda_model_api::ModelApiFactory;
use barracuda_net::{Dns, TcpConnect};

use crate::config::SharedApiManager;
use crate::context_providers::LongTermMemoryContextProvider;

use super::layout::join_storage_path;

const GLOBAL_LONG_TERM_DIR: &str = "g";
const AGENT_LONG_TERM_DIR: &str = "a";

type BuildProvider<F> =
    dyn Fn(LongTermMemory<F>, LongTermMemory<F>) -> LongTermMemoryContextProvider<F>;

pub(super) struct LongTermDeps<F: FileSystem + 'static> {
    global: LongTermMemory<F>,
    agent_stores: AgentMemoryStores<F>,
    build_provider: Arc<BuildProvider<F>>,
}

struct AgentMemoryStores<F: FileSystem + 'static> {
    filesystem: F,
    root_dir: String,
    by_kind: RefCell<BTreeMap<String, LongTermMemory<F>>>,
}

impl<F: FileSystem + 'static> AgentMemoryStores<F> {
    fn new(filesystem: F, root_dir: String) -> Self {
        Self {
            filesystem,
            root_dir,
            by_kind: RefCell::new(BTreeMap::new()),
        }
    }

    fn get(&self, kind: &str) -> Result<LongTermMemory<F>, LongTermInitError> {
        let mut stores = self.by_kind.borrow_mut();
        if let Some(store) = stores.get(kind) {
            return Ok(store.clone());
        }

        let dir = join_storage_path(&self.root_dir, kind);
        let store =
            LongTermMemoryContextProvider::<F>::open_agent_store(self.filesystem.clone(), &dir)?;
        stores.insert(kind.to_owned(), store.clone());
        Ok(store)
    }
}

impl<F: FileSystem + 'static> LongTermDeps<F> {
    pub(super) fn from_root<H>(
        filesystem: F,
        long_term_dir: &str,
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<H>,
    ) -> Result<Self, LongTermInitError>
    where
        H: TcpConnect + Dns + 'static,
    {
        let global_dir = join_storage_path(long_term_dir, GLOBAL_LONG_TERM_DIR);
        let agent_root_dir = join_storage_path(long_term_dir, AGENT_LONG_TERM_DIR);
        Ok(Self {
            global: LongTermMemoryContextProvider::<F>::open_global_store(
                filesystem.clone(),
                &global_dir,
            )?,
            agent_stores: AgentMemoryStores::new(filesystem, agent_root_dir),
            build_provider: LongTermMemoryContextProvider::<F>::llm_builder::<H>(
                api_manager,
                llm_factory,
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
    use barracuda_agent_memory::{MemoryDraft, StoreOutcome};
    use barracuda_platform_test::MemFs;

    use super::AgentMemoryStores;

    #[test]
    fn same_kind_reuses_one_live_store_owner() {
        let stores = AgentMemoryStores::<MemFs>::new(MemFs::new(), "/memory/a".to_owned());
        let first = stores.get("conversation").expect("first store opens");
        let second = stores.get("conversation").expect("second store opens");

        assert!(matches!(
            first.store(MemoryDraft::new("fact from first agent")),
            StoreOutcome::Created(_)
        ));
        assert_eq!(second.list().len(), 1);
    }
}
