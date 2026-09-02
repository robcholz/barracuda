#![allow(missing_docs)]

use std::process::Command;

fn cargo_plugin(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-plugin"))
        .args(args)
        .output()
        .expect("cargo-plugin binary starts")
}

#[test]
fn info_reports_real_dependency_and_status_metadata() {
    let output = cargo_plugin(&["info", "gateway-agent"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("output is UTF-8");
    assert!(stdout.contains("gateway-agent"));
    assert!(stdout.contains("depends on"));
    assert!(stdout.contains("agent, imessage-gateway"));
    assert!(stdout.contains("status"));
}

#[test]
fn info_unknown_plugin_fails_with_an_actionable_diagnostic() {
    let output = cargo_plugin(&["info", "does-not-exist"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(stderr.contains("unknown Plugin `does-not-exist`"));
}

#[test]
fn sync_check_validates_the_checked_in_registry_without_writing() {
    let output = cargo_plugin(&["sync", "--check"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("output is UTF-8");
    assert!(stdout.contains("Checked Plugin registry"));
    assert!(stdout.contains("enabled"));
    assert!(stdout.contains("disabled"));
}
