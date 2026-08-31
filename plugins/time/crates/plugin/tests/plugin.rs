//! Time Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
use barracuda_time_plugin::TimePlugin;
use futures_lite::future::block_on;

#[test]
fn plugin_loads_the_time_component() {
    block_on(async {
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create database partition");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let mut router = EventRouter::new(lanes).await.expect("create router");
        let id = PluginId::try_from("time").expect("valid Plugin ID");
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        let plugin = TimePlugin::new(&mut context);

        assert_eq!(TimePlugin::id(), "time");
        assert!(<TimePlugin as Plugin<512>>::DEPENDS_ON.is_empty());
        manager
            .register(&mut router, plugin)
            .expect("register Time Plugin");
        manager.start(&mut router).expect("start Plugins");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    });
}
