//! Plugin storage must not expose its database's physical flash backend.

extern crate alloc;

use barracuda_plugin_manager::{Plugin, PluginDeclaration};

struct StorageAgnosticPlugin;

impl PluginDeclaration for StorageAgnosticPlugin {
    const ID: &'static str = "storage-agnostic";
}

impl Plugin for StorageAgnosticPlugin {}

#[test]
fn plugin_contract_is_generic_over_semantic_storage_not_flash() {
    fn assert_plugin<T: Plugin>() {}

    assert_plugin::<StorageAgnosticPlugin>();
}
