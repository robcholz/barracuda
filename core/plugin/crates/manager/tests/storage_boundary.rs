//! Plugin storage must not expose its database's physical flash backend.

extern crate alloc;

use barracuda_plugin_manager::{Plugin, PluginDeclaration};

struct StorageAgnosticPlugin;

impl PluginDeclaration for StorageAgnosticPlugin {
    const ID: &'static str = "storage-agnostic";
}

impl<const M: usize> Plugin<M> for StorageAgnosticPlugin {}

#[test]
fn plugin_contract_is_generic_over_semantic_storage_not_flash() {
    fn assert_plugin<T: Plugin<64>>() {}

    assert_plugin::<StorageAgnosticPlugin>();
}

#[test]
fn production_plugins_do_not_reference_flash_contracts() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let plugin_files = [
        "plugins/agent/crates/plugin/src/lib.rs",
        "plugins/imessage-bluebubble/crates/plugin/src/lib.rs",
        "plugins/imessage-inkbox/crates/plugin/src/lib.rs",
        "plugins/imessage-gateway/crates/plugin/src/lib.rs",
        "plugins/imessage-telegram/crates/plugin/src/lib.rs",
        "plugins/imessage-web/crates/plugin/src/lib.rs",
        "plugins/imessage-wechat/crates/plugin/src/lib.rs",
        "plugins/scheduler/crates/plugin/src/lib.rs",
        "plugins/time/crates/plugin/src/lib.rs",
        "plugins/vm/crates/plugin/src/lib.rs",
        "plugins/webserver/crates/plugin/src/lib.rs",
    ];

    for relative in plugin_files {
        let source = std::fs::read_to_string(root.join(relative))?;
        assert!(
            !source.contains("PartitionFlash"),
            "{relative} leaks partition flash"
        );
        assert!(!source.contains("NorFlash"), "{relative} leaks NOR flash");
    }
    Ok(())
}
