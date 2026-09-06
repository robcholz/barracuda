//! Outbound HTTP JSON RPC Plugin.
#![no_std]

extern crate alloc;

mod component;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};
use http_client::ClientFactory;

use component::HttpComponent;

/// Plugin exposing the shared HTTP client through JSON RPC.
#[barracuda_plugin_api::plugin]
pub struct HttpPlugin {
    clients: ClientFactory<'static>,
}
impl HttpPlugin {
    /// Creates a Plugin from System's shared HTTP client factory.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            clients: context.http_clients.clone(),
        }
    }
}
impl<const M: usize> Plugin<M> for HttpPlugin {
    fn register<S: barracuda_plugin_manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, S>,
    ) -> PluginResult<()> {
        context
            .event_router
            .load(HttpComponent::new(self.clients.clone()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack,
    };
    use barracuda_plugin_api::{ClientFactory, PluginContext};
    use barracuda_plugin_manager::{PluginDeclaration, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use super::HttpPlugin;

    #[test]
    fn plugin_loads_and_unloads_its_http_component() {
        block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create Plugin storage partition");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            install_global_memory_vfs()
                .await
                .expect("install global test VFS");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create Event Router");
            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            let id = PluginId::try_from("http").expect("valid Plugin ID");

            assert_eq!(<HttpPlugin as PluginDeclaration>::ID, "http");
            assert!(<HttpPlugin as PluginDeclaration>::DEPENDS_ON.is_empty());
            manager
                .register(&mut router, HttpPlugin::new(&mut context))
                .expect("register HTTP Plugin");
            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));

            manager
                .unload(&mut router, &id)
                .await
                .expect("unload HTTP Plugin");
            assert!(manager.component_ids(&id).is_none());
        });
    }
}
