//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;

mod model_api_http;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig};
use barracuda_model_api::ModelApi;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_webserver_plugin::WebServer;
use http_client::ClientFactory;

pub use model_api_http::SET_API_PATH;

const PERSISTENCE_ROOT: &str = "/";

/// Plugin that constructs and owns the Agent runtime and Component.
#[barracuda_plugin::macros::plugin]
pub struct AgentPlugin {
    http_clients: ClientFactory<'static>,
}

impl AgentPlugin {
    /// Creates the Plugin with Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl<const M: usize> Plugin<M> for AgentPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let filesystem = context.filesystem()?.clone();
        let http_clients = self.http_clients.clone();
        let model_api_factory = ModelApiFactory::new(move || ModelApi::new(http_clients.clone()));
        let storage = RuntimeStorageConfig {
            persistence_root: PERSISTENCE_ROOT.into(),
            skill_roots: Vec::new(),
        };
        let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
            .map_err(PluginError::registration)?;
        runtime.start_all().map_err(PluginError::registration)?;
        let runtime = Rc::new(runtime);
        let registration = webserver
            .serve_http(
                SET_API_PATH,
                model_api_http::SetApiEndpoint::new(Rc::clone(&runtime)),
            )
            .map_err(PluginError::registration)?;
        context.retain(registration);
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
    use alloc::rc::Rc;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, memory_vfs_root, never_embassy_stack,
    };
    use barracuda_plugin::api::PluginContext;
    use barracuda_plugin::manager::{
        Plugin, PluginDeclaration, PluginId, PluginManager, PluginRegisterContext, PluginResult,
    };
    use barracuda_webserver_plugin::WebServer;
    use futures_lite::future::block_on;

    use http_client::ClientFactory;

    use super::AgentPlugin;

    struct WebServerProvider(Rc<WebServer>);

    impl PluginDeclaration for WebServerProvider {
        const ID: &'static str = "webserver";
    }

    impl Plugin<512> for WebServerProvider {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, 512, Storage>,
        ) -> PluginResult<()>
        where
            Storage: barracuda_plugin::manager::PluginStorage,
        {
            context.provide(Rc::clone(&self.0))?;
            Ok(())
        }
    }

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

            manager
                .register(&mut router, WebServerProvider(Rc::new(WebServer::new())))
                .expect("register WebServer provider");

            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            let plugin = AgentPlugin::new(&mut context);
            assert_eq!(
                <AgentPlugin as barracuda_plugin::manager::PluginDeclaration>::ID,
                "agent"
            );

            manager
                .register(&mut router, plugin)
                .expect("register Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
