//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::AgentRuntime;
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};

pub use barracuda_agent_runtime::{ModelApiFactory, RuntimeStorageConfig};

/// Stable identity of the Agent Plugin.
pub const PLUGIN_ID: &str = "agent";

/// Plugin that constructs and owns the Agent runtime and Component.
pub struct AgentPlugin<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    filesystem: Option<Filesystem>,
    storage: Option<RuntimeStorageConfig>,
    model_api_factory: Option<ModelApiFactory<Http>>,
}

impl<Filesystem, Http> AgentPlugin<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    /// Creates the Plugin from low-level abstract capabilities.
    #[must_use]
    pub fn new(
        filesystem: Filesystem,
        storage: RuntimeStorageConfig,
        model_api_factory: ModelApiFactory<Http>,
    ) -> Self {
        Self {
            filesystem: Some(filesystem),
            storage: Some(storage),
            model_api_factory: Some(model_api_factory),
        }
    }
}

impl<Filesystem, Http, const M: usize> Plugin<M> for AgentPlugin<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            let filesystem = self
                .filesystem
                .take()
                .ok_or_else(|| PluginError::registration(AlreadyRegistered))?;
            let storage = self
                .storage
                .take()
                .ok_or_else(|| PluginError::registration(AlreadyRegistered))?;
            let model_api_factory = self
                .model_api_factory
                .take()
                .ok_or_else(|| PluginError::registration(AlreadyRegistered))?;
            let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
                .map_err(PluginError::registration)?;
            runtime.start_all().map_err(PluginError::registration)?;
            context.load(AgentComponent::new(runtime, service))?;
            Ok(())
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Agent Plugin has already registered its Component")]
struct AlreadyRegistered;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use barracuda_agent_runtime::{ModelApiFactory, RuntimeStorageConfig};
    use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
    use barracuda_model_api::ModelApi;
    use barracuda_net::testing::NeverStack;
    use barracuda_plugin_manager::{EkvStore, NoopRawMutex, Plugin, PluginId, PluginManager};
    use ekv::{flash::MemFlash, Config};
    use futures_lite::future::block_on;

    use super::AgentPlugin;

    static NETWORK: NeverStack = NeverStack;

    #[test]
    fn plugin_loads_its_agent_component() {
        let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
        block_on(store.format()).expect("format store");
        let mut manager = PluginManager::new(store);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        let id = PluginId::try_from("agent").expect("valid Plugin ID");

        let model_api_factory = ModelApiFactory::new(|| ModelApi::new(&NETWORK, 1024, 1024));
        let plugin = AgentPlugin::new(
            MemFs::new(),
            RuntimeStorageConfig {
                persistence_root: "/agent".into(),
                skill_roots: Vec::new(),
            },
            model_api_factory,
        );
        assert_eq!(Plugin::<512>::id(&plugin), "agent");

        block_on(manager.register(&mut router, plugin)).expect("register Agent Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
