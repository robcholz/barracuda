//! Scheduler Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use std::rc::Rc;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{MemFs, NeverStack, memory_partition};
use barracuda_plugin_manager::{Plugin, PluginId, PluginManager, PluginRegisterError};
use barracuda_scheduler_plugin::{PLUGIN_ID, SchedulerPlugin};
use barracuda_time_plugin::TimePlugin;
use futures_lite::future::block_on;

static NETWORK: NeverStack = NeverStack;

#[test]
fn plugin_requires_time_and_loads_the_scheduler_component() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager
        .provide_system(Rc::new(&NETWORK))
        .expect("provide network");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
    let filesystem = MemFs::new();
    let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
    let scheduler = || SchedulerPlugin;

    assert_eq!(Plugin::<512>::id(&scheduler()), "scheduler");
    assert_eq!(
        <SchedulerPlugin as Plugin<512>>::DEPENDS_ON,
        &[barracuda_time_plugin::PLUGIN_ID],
    );
    let error = block_on(manager.register(&mut router, scheduler()))
        .expect_err("reject Scheduler before Time");
    assert!(matches!(error, PluginRegisterError::MissingDependency(id) if id.as_str() == "time"));

    block_on(manager.register(&mut router, TimePlugin::<NeverStack>::default()))
        .expect("register Time Plugin");
    block_on(manager.register(&mut router, scheduler())).expect("register Scheduler Plugin");
    block_on(manager.start(&mut router)).expect("start Plugins");

    let id = PluginId::try_from(PLUGIN_ID).expect("valid Plugin ID");
    assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
}
