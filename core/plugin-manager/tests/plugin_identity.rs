//! Plugin-owned identity integration tests.

#![allow(clippy::expect_used)]

use std::boxed::Box;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{memory_partition, MemFs, MemoryPartition};
use barracuda_plugin_manager::{
    Plugin, PluginContext, PluginId, PluginIdError, PluginManager, PluginRegisterError,
    PluginStartFuture,
};
use futures_lite::future::block_on;

const FRAME_SIZE: usize = 64;

struct IdentifiedPlugin(&'static str);

impl Plugin<FRAME_SIZE> for IdentifiedPlugin {
    fn id(&self) -> &'static str {
        self.0
    }

    fn start<'a, Storage>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async { Ok(()) })
    }
}

fn manager() -> PluginManager<FRAME_SIZE, MemoryPartition> {
    block_on(async {
        let partition = memory_partition(MAX_CAPACITY)
            .await
            .expect("create database partition");
        PluginManager::open(partition)
            .await
            .expect("open Plugin storage")
    })
}

fn router() -> EventRouter<4, FRAME_SIZE, 4> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let filesystem = MemFs::new();
    EventRouter::new(lanes, filesystem, "workflows").expect("create Event Router")
}

#[test]
fn manager_uses_the_identity_declared_by_the_plugin() {
    let mut manager = manager();
    let mut router = router();
    let id = PluginId::try_from("identified").expect("valid Plugin ID");

    block_on(manager.register(&mut router, IdentifiedPlugin("identified")))
        .expect("register Plugin");

    assert!(manager.is_loaded(&id));
}

#[test]
fn manager_rejects_an_invalid_plugin_identity() {
    let mut manager = manager();
    let mut router = router();

    let error = block_on(manager.register(&mut router, IdentifiedPlugin("")))
        .expect_err("reject invalid Plugin ID");

    assert!(matches!(
        error,
        PluginRegisterError::InvalidId(PluginIdError::Empty)
    ));
}

#[test]
fn manager_rejects_a_duplicate_plugin_identity() {
    let mut manager = manager();
    let mut router = router();
    let id = PluginId::try_from("duplicate").expect("valid Plugin ID");

    block_on(manager.register(&mut router, IdentifiedPlugin("duplicate")))
        .expect("register Plugin");
    let error = block_on(manager.register(&mut router, IdentifiedPlugin("duplicate")))
        .expect_err("reject duplicate Plugin ID");

    assert!(matches!(error, PluginRegisterError::AlreadyRegistered(found) if found == id));
}
