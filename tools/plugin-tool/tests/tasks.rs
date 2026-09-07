//! CLI task selection and fail-fast behavior use the production runner.
#![allow(clippy::unwrap_used)]

use barracuda_plugin_tool::run;
use std::{fs, path::Path};

fn plugin(root: &Path, id: &str, script: Option<&str>) {
    let directory = root.join("plugins").join(id);
    fs::create_dir_all(directory.join("crates/plugin/src")).unwrap();
    fs::write(
        directory.join("crates/plugin/Cargo.toml"),
        format!("[package]\nname = \"barracuda-{id}-plugin\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    fs::write(
        directory.join("crates/plugin/src/lib.rs"),
        "#[barracuda_plugin::macros::plugin]\npub struct DemoPlugin;\nimpl Plugin for DemoPlugin {}\n",
    )
    .unwrap();
    let mut manifest = format!("id = '{id}'\ndescription = 'Test.'\ndepends-on = []\n");
    if let Some(script) = script {
        manifest.push_str(&format!(
            "[tasks.build]\ncommand = ['sh', '-c', {script:?}]\n"
        ));
    }
    fs::write(directory.join("plugin.toml"), manifest).unwrap();
}

#[test]
fn batch_skips_disabled_and_missing_tasks_and_stops_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    plugin(root, "a", Some("touch built"));
    plugin(root, "b", Some("touch built"));
    plugin(root, "c", None);
    fs::create_dir(root.join(".barracuda")).unwrap();
    fs::write(root.join(".barracuda/disabled-plugins"), "b\n").unwrap();
    run(root, "build", None).unwrap();
    assert!(root.join("plugins/a/built").exists());
    assert!(!root.join("plugins/b/built").exists());
    assert!(run(root, "build", Some("b")).is_err());
    assert!(run(root, "build", Some("c")).is_err());
    assert!(run(root, "build", Some("unknown")).is_err());
    plugin(root, "a", Some("exit 4"));
    plugin(root, "c", Some("touch built"));
    assert!(run(root, "build", None).is_err());
    assert!(!root.join("plugins/c/built").exists());
}
