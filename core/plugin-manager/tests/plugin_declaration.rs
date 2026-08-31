//! Static Plugin declaration contract tests.

#![allow(clippy::expect_used)]

use barracuda_plugin_manager::{Plugin, PluginDeclaration};

struct DeclaredPlugin;

impl PluginDeclaration for DeclaredPlugin {
    const ID: &'static str = "declared";
    const DEPENDS_ON: &'static [&'static str] = &["provider"];
}

impl Plugin<64> for DeclaredPlugin {}

#[test]
fn declaration_is_independent_of_frame_size() {
    assert_eq!(DeclaredPlugin::ID, "declared");
    assert_eq!(DeclaredPlugin::DEPENDS_ON, ["provider"]);
}
