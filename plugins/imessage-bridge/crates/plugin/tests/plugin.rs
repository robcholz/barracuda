//! iMessage Bridge Plugin lifecycle contract.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_imessage_bridge_plugin::ImessageBridgePlugin;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{PluginDeclaration, PluginId, PluginManager};

#[test]
fn plugin_loads_one_component_without_typed_dependencies() {
    futures_lite::future::block_on(async {
        install_global_memory_vfs().await.expect("install test VFS");
        let partition = memory_partition(64 * 1024).await.expect("partition");
        let mut manager = PluginManager::open(partition).await.expect("manager");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let mut router = EventRouter::new(lanes).await.expect("router");
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));

        assert_eq!(
            <ImessageBridgePlugin as PluginDeclaration>::ID,
            "imessage-bridge"
        );
        assert_eq!(
            <ImessageBridgePlugin as PluginDeclaration>::DEPENDS_ON,
            &[] as &[&str]
        );
        manager
            .register(&mut router, ImessageBridgePlugin::new(&mut context))
            .expect("register bridge Plugin");
        let id = PluginId::try_from("imessage-bridge").expect("Plugin ID");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        manager
            .unload(&mut router, &id)
            .await
            .expect("unload bridge Plugin");
    });
}
