//! Time Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use std::rc::Rc;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{MemFs, NeverStack, memory_partition};
use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
use barracuda_time_plugin::{PLUGIN_ID, TimePlugin};
use futures_lite::future::block_on;

static NETWORK: NeverStack = NeverStack;

#[test]
fn plugin_loads_the_time_component() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager
        .provide_system(Rc::new(&NETWORK))
        .expect("provide network");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
    let filesystem = MemFs::new();
    let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
    let id = PluginId::try_from(PLUGIN_ID).expect("valid Plugin ID");
    let plugin = TimePlugin::<NeverStack>::default();

    assert_eq!(Plugin::<512>::id(&plugin), "time");
    assert!(<TimePlugin<NeverStack> as Plugin<512>>::DEPENDS_ON.is_empty());
    block_on(manager.register(&mut router, plugin)).expect("register Time Plugin");
    block_on(manager.start(&mut router)).expect("start Plugins");
    assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
}
