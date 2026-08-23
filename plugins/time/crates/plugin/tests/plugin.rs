//! Time Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
use barracuda_net::testing::NeverStack;
use barracuda_plugin_manager::{EkvStore, NoopRawMutex, Plugin, PluginId, PluginManager};
use barracuda_time_plugin::{PLUGIN_ID, TimePlugin};
use ekv::{Config, flash::MemFlash};
use futures_lite::future::block_on;

#[test]
fn plugin_loads_the_time_component() {
    let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
    block_on(store.format()).expect("format store");
    let mut manager = PluginManager::new(store);
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
    let id = PluginId::try_from(PLUGIN_ID).expect("valid Plugin ID");
    let plugin = TimePlugin::new(NeverStack);

    assert_eq!(Plugin::<512>::id(&plugin), "time");
    assert!(<TimePlugin<NeverStack> as Plugin<512>>::DEPENDS_ON.is_empty());
    block_on(manager.register(&mut router, plugin)).expect("register Time Plugin");
    block_on(manager.start(&mut router)).expect("start Plugins");
    assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
}
