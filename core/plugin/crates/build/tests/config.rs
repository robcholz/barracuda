//! Board-configured Plugin settings.

#![allow(clippy::expect_used)]

use barracuda_plugin_build::render_config;
use barracuda_plugin_manifest::parse;

const MANIFEST: &str = r#"
id = "example"
depends-on = []
description = "Demonstrates settings."

[config.receive-slots]
type = "usize"
default = 1
description = "Open receive connections."

[config.label]
type = "str"
default = "device"
description = "Shown name."
"#;

fn board(plugins: &str) -> barracuda_board_config::BoardDefinition {
    barracuda_board_config::parse(&format!(
        "name: demo\nhardware:\n  chip: demo\nnative-layout:\n  artifact: layout.yml\n{plugins}"
    ))
    .expect("Board")
}

#[test]
fn defaults_apply_without_a_board_setting() {
    let manifest = parse(MANIFEST).expect("manifest");

    assert_eq!(
        render_config(&manifest, None).expect("defaults"),
        concat!(
            "/// Shown name.\n",
            "pub const LABEL: &str = \"device\";\n",
            "/// Open receive connections.\n",
            "pub const RECEIVE_SLOTS: usize = 1;\n",
        )
    );
}

#[test]
fn the_board_overrides_its_plugin_settings_only() {
    let manifest = parse(MANIFEST).expect("manifest");
    let board =
        board("plugins:\n  example:\n    receive-slots: 3\n  other:\n    receive-slots: 9\n");

    let rendered = render_config(&manifest, Some(&board)).expect("settings");
    assert!(rendered.contains("pub const RECEIVE_SLOTS: usize = 3;"));
    assert!(rendered.contains("pub const LABEL: &str = \"device\";"));
}

#[test]
fn rejects_undeclared_and_mistyped_board_settings() {
    let manifest = parse(MANIFEST).expect("manifest");

    for plugins in [
        "plugins:\n  example:\n    slots: 3\n",
        "plugins:\n  example:\n    receive-slots: kitchen\n",
        "plugins:\n  example:\n    receive-slots: -1\n",
    ] {
        assert!(render_config(&manifest, Some(&board(plugins))).is_err());
    }
}
