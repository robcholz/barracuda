//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::marker::PhantomData;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, RuntimeStorageConfig};
use barracuda_fs::FileSystem;
use barracuda_model_api::ModelApi;
use barracuda_net::{Dns, TcpConnect};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};

pub use barracuda_agent_runtime::{ApiPurpose, ModelApiConfig, ModelApiFactory};
pub use barracuda_model_api::{BackendKind, InitError};

const PERSISTENCE_ROOT: &str = "/agent";
const HTTP_HEADER_BYTES: usize = 16 * 1024;
const HTTP_READ_BYTES: usize = 8 * 1024;
/// Stable identity of the Agent Plugin.
pub const PLUGIN_ID: &str = "agent";

type SetApiHandler = dyn Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError>;

/// Typed capability for configuring the Agent's model APIs.
pub struct AgentSetApi {
    handler: Box<SetApiHandler>,
}

impl AgentSetApi {
    /// Creates a model API configuration capability around one synchronous handler.
    #[must_use]
    pub fn new(
        handler: impl Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError> + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// Applies one model API configuration.
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] when the configuration is invalid.
    pub fn set_api(
        &self,
        api: ModelApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        (self.handler)(api, purpose, default)
    }
}

/// Plugin that constructs and owns the Agent runtime and Component.
pub struct AgentPlugin<Filesystem, Network>
where
    Filesystem: FileSystem + 'static,
    Network: TcpConnect + Dns + 'static,
{
    capabilities: PhantomData<fn() -> (Filesystem, Network)>,
}

impl<Filesystem, Network> Default for AgentPlugin<Filesystem, Network>
where
    Filesystem: FileSystem + 'static,
    Network: TcpConnect + Dns + 'static,
{
    fn default() -> Self {
        Self {
            capabilities: PhantomData,
        }
    }
}

impl<Filesystem, Network, const M: usize> Plugin<M> for AgentPlugin<Filesystem, Network>
where
    Filesystem: FileSystem + 'static,
    Network: TcpConnect + Dns + 'static,
{
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let filesystem = context.require_system::<Filesystem>()?;
            let filesystem = filesystem.as_ref().clone();
            let network = context.require_system::<&'static Network>()?;
            let network = *network;
            #[cfg(feature = "mbedtls-host")]
            let host_tls = barracuda_model_api::HostTls::from_system_certificates()
                .map_err(PluginError::registration)?;
            #[cfg(feature = "mbedtls-host")]
            let model_api_factory = ModelApiFactory::new(move || {
                ModelApi::new_with_tls(
                    network,
                    host_tls.config(),
                    HTTP_HEADER_BYTES,
                    HTTP_READ_BYTES,
                )
            });
            #[cfg(not(feature = "mbedtls-host"))]
            let model_api_factory = ModelApiFactory::new(move || {
                ModelApi::new(network, HTTP_HEADER_BYTES, HTTP_READ_BYTES)
            });
            let storage = RuntimeStorageConfig {
                persistence_root: PERSISTENCE_ROOT.into(),
                skill_roots: Vec::new(),
            };
            let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
                .map_err(PluginError::registration)?;
            runtime.start_all().map_err(PluginError::registration)?;
            let runtime = Rc::new(runtime);
            let set_api_runtime = Rc::clone(&runtime);
            context.provide(Rc::new(AgentSetApi::new(move |api, purpose, default| {
                set_api_runtime.set_api(api, purpose, default)
            })))?;
            context.load(AgentComponent::from_shared(runtime, service))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;

    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{memory_partition, MemFs, NeverStack};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use super::AgentPlugin;

    static NETWORK: NeverStack = NeverStack;

    #[test]
    fn plugin_loads_its_agent_component() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
        let filesystem = MemFs::new();
        let mut router =
            EventRouter::new(lanes, filesystem.clone(), "workflows").expect("create router");
        let id = PluginId::try_from("agent").expect("valid Plugin ID");

        manager
            .provide_system(Rc::new(filesystem))
            .expect("provide filesystem");
        manager
            .provide_system(Rc::new(&NETWORK))
            .expect("provide network");
        let plugin = AgentPlugin::<MemFs, NeverStack>::default();
        assert_eq!(Plugin::<512>::id(&plugin), "agent");

        block_on(manager.register(&mut router, plugin)).expect("register Agent Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
