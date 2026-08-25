//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, RuntimeStorageConfig};
use barracuda_model_api::ModelApi;
use barracuda_plugin_manager::{
    Plugin, PluginContext, PluginError, PluginFilesystem, PluginRequirements, PluginResult,
};
use http_client::ClientFactory;

pub use barracuda_agent_runtime::{ApiPurpose, ModelApiConfig, ModelApiFactory};
pub use barracuda_model_api::{BackendKind, InitError};

const PERSISTENCE_ROOT: &str = "/";
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
pub struct AgentPlugin {
    http: ClientFactory,
}

impl AgentPlugin {
    /// Creates the Plugin with the System-owned HTTP client factory.
    #[must_use]
    pub const fn new(http: ClientFactory) -> Self {
        Self { http }
    }
}

impl<const M: usize> Plugin<M> for AgentPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let filesystem = context.filesystem()?.clone();
        let http = self.http.clone();
        let model_api_factory = ModelApiFactory::new(move || ModelApi::new(http.create()));
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
        context
            .event_router
            .load(AgentComponent::from_shared(runtime, service))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, memory_vfs_root, never_embassy_stack,
    };
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use http_client::ClientFactory;

    use super::AgentPlugin;

    #[test]
    fn plugin_loads_its_agent_component() {
        block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create database partition");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            manager.install_vfs(memory_vfs_root().await.expect("create System VFS"));
            install_global_memory_vfs()
                .await
                .expect("install global test VFS");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let id = PluginId::try_from("agent").expect("valid Plugin ID");

            let plugin = AgentPlugin::new(ClientFactory::plaintext(never_embassy_stack()));
            assert_eq!(Plugin::<512>::id(&plugin), "agent");

            manager
                .register(&mut router, plugin)
                .expect("register Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
