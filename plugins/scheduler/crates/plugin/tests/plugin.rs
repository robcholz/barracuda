//! Scheduler Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{Plugin, PluginId, PluginManager, PluginRegisterError};
use barracuda_scheduler_plugin::SchedulerPlugin;
use barracuda_time_plugin::TimePlugin;
use futures_lite::future::block_on;

#[test]
fn plugin_requires_time_and_loads_the_scheduler_component() {
    block_on(async {
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create database partition");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let mut router = EventRouter::new(lanes).await.expect("create router");
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));

        assert_eq!(
            Plugin::<512>::id(&SchedulerPlugin::new(&mut context)),
            "scheduler"
        );
        assert_eq!(<SchedulerPlugin as Plugin<512>>::DEPENDS_ON, &["time"],);
        let error = manager
            .register(&mut router, SchedulerPlugin::new(&mut context))
            .expect_err("reject Scheduler before Time");
        assert!(
            matches!(error, PluginRegisterError::MissingDependency(id) if id.as_str() == "time")
        );

        manager
            .register(&mut router, TimePlugin::new(&mut context))
            .expect("register Time Plugin");
        manager
            .register(&mut router, SchedulerPlugin::new(&mut context))
            .expect("register Scheduler Plugin");
        manager.start(&mut router).expect("start Plugins");

        let id = PluginId::try_from("scheduler").expect("valid Plugin ID");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    });
}
