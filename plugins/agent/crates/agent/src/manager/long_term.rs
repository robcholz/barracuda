use alloc::{borrow::ToOwned, collections::BTreeMap, string::String};
use core::cell::RefCell;

use barracuda_agent_memory::{LongTermInitError, LongTermMemory};
use barracuda_model_api::ModelApiFactory;
use barracuda_vfs::ScopedVfs;
use http_client::embedded_nal_async::{Dns, TcpConnect};
use portable_atomic_util::Arc;

use crate::config::SharedApiManager;
use crate::context_providers::LongTermMemoryContextProvider;

use super::layout::join_storage_path;

const GLOBAL_LONG_TERM_DIR: &str = "g";
const AGENT_LONG_TERM_DIR: &str = "a";

type BuildProvider = dyn Fn(LongTermMemory, LongTermMemory) -> LongTermMemoryContextProvider;

pub(super) struct LongTermDeps {
    global: LongTermMemory,
    agent_stores: AgentMemoryStores,
    build_provider: Arc<BuildProvider>,
}

struct AgentMemoryStores {
    filesystem: ScopedVfs,
    root_dir: String,
    by_kind: RefCell<BTreeMap<String, LongTermMemory>>,
}

impl AgentMemoryStores {
    fn new(filesystem: ScopedVfs, root_dir: String) -> Self {
        Self {
            filesystem,
            root_dir,
            by_kind: RefCell::new(BTreeMap::new()),
        }
    }

    async fn preload(&self, kind: &str) -> Result<(), LongTermInitError> {
        let dir = join_storage_path(&self.root_dir, kind);
        let store =
            LongTermMemoryContextProvider::open_agent_store(self.filesystem.clone(), &dir).await?;
        self.by_kind.borrow_mut().insert(kind.to_owned(), store);
        Ok(())
    }

    fn get(&self, kind: &str) -> Option<LongTermMemory> {
        self.by_kind.borrow().get(kind).cloned()
    }
}

impl LongTermDeps {
    pub(super) async fn from_root<Tcp, Resolver>(
        filesystem: ScopedVfs,
        long_term_dir: &str,
        kinds: impl IntoIterator<Item = &'static str>,
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
    ) -> Result<Self, LongTermInitError>
    where
        Tcp: TcpConnect + 'static,
        Resolver: Dns + 'static,
    {
        let global_dir = join_storage_path(long_term_dir, GLOBAL_LONG_TERM_DIR);
        let agent_root_dir = join_storage_path(long_term_dir, AGENT_LONG_TERM_DIR);
        let global =
            LongTermMemoryContextProvider::open_global_store(filesystem.clone(), &global_dir)
                .await?;
        let agent_stores = AgentMemoryStores::new(filesystem, agent_root_dir);
        for kind in kinds {
            agent_stores.preload(kind).await?;
        }
        Ok(Self {
            global,
            agent_stores,
            build_provider: LongTermMemoryContextProvider::llm_builder(api_manager, llm_factory),
        })
    }

    pub(super) fn provider(
        &self,
        kind: &str,
    ) -> Result<LongTermMemoryContextProvider, LongTermInitError> {
        let agent = self.agent_stores.get(kind).ok_or_else(|| {
            // Every baked kind is preloaded during manager construction.
            LongTermInitError::Unreadable {
                path: kind.to_owned(),
                source: barracuda_vfs::FsError::NotFound,
            }
        })?;
        Ok((self.build_provider)(agent, self.global.clone()))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_agent_memory::{MemoryDraft, StoreOutcome};
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;

    use super::AgentMemoryStores;

    #[test]
    fn same_kind_reuses_one_live_store_owner() {
        block_on(async {
            let stores = AgentMemoryStores::new(
                memory_vfs().await.expect("memory VFS mounts"),
                "/memory/a".to_owned(),
            );
            stores
                .preload("conversation")
                .await
                .expect("store preloads");
            let first = stores.get("conversation").expect("first store opens");
            let second = stores.get("conversation").expect("second store opens");

            assert!(matches!(
                first.store(MemoryDraft::new("fact from first agent")).await,
                StoreOutcome::Created(_)
            ));
            assert_eq!(second.list().len(), 1);
        });
    }
}
