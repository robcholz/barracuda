//! Real host command execution tests.
#![allow(clippy::unwrap_used)]

use barracuda_plugin_build::run;
use std::{fs, path::Path};

fn manifest(root: &Path, task: &str) {
    fs::write(
        root.join("plugin.toml"),
        format!("id = 'demo'\ndescription = 'Test.'\ndepends-on = []\n[tasks.build]\n{task}\n"),
    )
    .unwrap();
}

#[test]
fn executes_literal_arguments_in_plugin_cwd_and_checks_outputs() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("resources")).unwrap();
    manifest(root.path(), "cwd = 'resources'\ncommand = ['sh', '-c', 'printf %s \"$1\" > result', '--', 'literal $HOME; space']\noutputs = ['resources/result']");
    assert!(run(root.path(), "build").unwrap());
    assert_eq!(
        fs::read_to_string(root.path().join("resources/result")).unwrap(),
        "literal $HOME; space"
    );
    assert!(!run(root.path(), "missing").unwrap());
    manifest(root.path(), "command = ['sh', '-c', 'exit 7']");
    assert!(run(root.path(), "build")
        .unwrap_err()
        .to_string()
        .contains("7"));
    manifest(
        root.path(),
        "command = ['sh', '-c', 'true']\noutputs = ['missing']",
    );
    assert!(run(root.path(), "build").is_err());
    manifest(root.path(), "command = ['barracuda-nonexistent-tool']");
    assert!(run(root.path(), "build").is_err());
}
