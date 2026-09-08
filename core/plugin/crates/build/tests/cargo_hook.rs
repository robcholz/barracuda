//! End-to-end Cargo build/run and incremental task tracking in an isolated package.
#![allow(clippy::unwrap_used)]

use std::{fs, path::Path, process::Command};

fn cargo(root: &Path, action: &str, value: &str) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args([action, "--offline"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("BARRACUDA_TASK_TEST_ENV", value)
        .output()
        .unwrap()
}

#[test]
fn cargo_build_and_run_track_inputs_outputs_and_environment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("resources")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("Cargo.toml"), format!("[package]\nname = 'task-hook-fixture'\nversion = '0.1.0'\nedition = '2021'\n[workspace]\n[build-dependencies]\nbarracuda-plugin-build = {{ path = {:?} }}\n", env!("CARGO_MANIFEST_DIR"))).unwrap();
    fs::write(root.join("build.rs"), "fn main() -> Result<(), barracuda_plugin_build::TaskError> { barracuda_plugin_build::build() }\n").unwrap();
    fs::write(root.join("plugin.toml"), "id = 'fixture'\ndescription = 'Task fixture.'\ndepends-on = []\n[tasks.build]\ncommand = ['sh', 'resources/build.sh']\ninputs = ['resources/build.sh', 'resources/input']\noutputs = ['result']\nenv = ['BARRACUDA_TASK_TEST_ENV']\n").unwrap();
    fs::write(root.join("resources/input"), "first").unwrap();
    fs::write(root.join("resources/build.sh"), "count=0\nif test -f count; then read -r count < count; fi\nprintf '%s\\n' \"$((count+1))\" > count\ncp resources/input result\n").unwrap();
    let first = cargo(root, "build", "first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(fs::read_to_string(root.join("count")).unwrap(), "1\n");
    let unchanged = cargo(root, "run", "first");
    assert!(unchanged.status.success());
    assert!(
        !String::from_utf8_lossy(&unchanged.stderr).contains("Compiling task-hook-fixture"),
        "unchanged cargo run rebuilt the fixture:\n{}",
        String::from_utf8_lossy(&unchanged.stderr)
    );
    assert_eq!(fs::read_to_string(root.join("count")).unwrap(), "1\n");
    fs::write(root.join("resources/input"), "second").unwrap();
    assert!(cargo(root, "build", "first").status.success());
    assert_eq!(fs::read_to_string(root.join("result")).unwrap(), "second");
    assert_eq!(fs::read_to_string(root.join("count")).unwrap(), "2\n");
    fs::remove_file(root.join("result")).unwrap();
    assert!(cargo(root, "build", "first").status.success());
    assert_eq!(fs::read_to_string(root.join("count")).unwrap(), "3\n");
    assert!(cargo(root, "build", "second").status.success());
    assert_eq!(fs::read_to_string(root.join("count")).unwrap(), "4\n");
    fs::write(root.join("resources/build.sh"), "exit 9\n").unwrap();
    let failed = cargo(root, "build", "second");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("fixture:build"));
}
