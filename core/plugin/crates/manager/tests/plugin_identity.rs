//! Plugin-owned identity integration tests.

#![allow(clippy::expect_used)]

use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{memory_partition, MemoryPartition};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginId, PluginIdError, PluginManager, PluginRegisterError,
};
use futures_lite::future::block_on;

struct IdentifiedPlugin;

impl PluginDeclaration for IdentifiedPlugin {
    const ID: &'static str = "identified";
}

impl Plugin for IdentifiedPlugin {}

struct InvalidIdentityPlugin;

impl PluginDeclaration for InvalidIdentityPlugin {
    const ID: &'static str = "";
}

impl Plugin for InvalidIdentityPlugin {}

struct DuplicatePlugin;

impl PluginDeclaration for DuplicatePlugin {
    const ID: &'static str = "duplicate";
}

impl Plugin for DuplicatePlugin {}

fn manager() -> PluginManager<MemoryPartition> {
    block_on(async {
        let partition = memory_partition(MAX_CAPACITY)
            .await
            .expect("create database partition");
        PluginManager::open(partition)
            .await
            .expect("open Plugin storage")
    })
}

#[test]
fn manager_uses_the_identity_declared_by_the_plugin() {
    let mut manager = manager();
    let id = PluginId::try_from("identified").expect("valid Plugin ID");

    manager.register(IdentifiedPlugin).expect("register Plugin");

    assert!(manager.is_loaded(&id));
}

#[test]
fn manager_rejects_an_invalid_plugin_identity() {
    let mut manager = manager();

    let error = manager
        .register(InvalidIdentityPlugin)
        .expect_err("reject invalid Plugin ID");

    assert!(matches!(
        error,
        PluginRegisterError::InvalidId(PluginIdError::Empty)
    ));
}

#[test]
fn manager_rejects_a_duplicate_plugin_identity() {
    let mut manager = manager();
    let id = PluginId::try_from("duplicate").expect("valid Plugin ID");

    manager.register(DuplicatePlugin).expect("register Plugin");
    let error = manager
        .register(DuplicatePlugin)
        .expect_err("reject duplicate Plugin ID");

    assert!(matches!(error, PluginRegisterError::AlreadyRegistered(found) if found == id));
}
