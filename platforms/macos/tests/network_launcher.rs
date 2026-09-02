//! Safe preflight behavior of the privileged macOS network launcher.

#![cfg(target_os = "macos")]

use std::process::Command;

fn launcher() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_barracuda-macos-network"));
    command.env_remove("SUDO_UID").env_remove("SUDO_GID");
    command
}

#[test]
fn launcher_requires_a_child_program_before_any_privileged_setup() {
    let output = launcher().output().expect("run network launcher");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(stderr.contains("usage: sudo barracuda-macos-network"));
}

#[test]
fn launcher_validates_the_complete_sudo_identity_before_opening_utun() {
    let missing_uid = launcher()
        .arg("/usr/bin/true")
        .output()
        .expect("run launcher without sudo identity");
    assert!(!missing_uid.status.success());
    assert!(String::from_utf8(missing_uid.stderr)
        .expect("diagnostic is UTF-8")
        .contains("SUDO_UID is missing"));

    let invalid_uid = launcher()
        .arg("/usr/bin/true")
        .env("SUDO_UID", "not-a-number")
        .env("SUDO_GID", "20")
        .output()
        .expect("run launcher with invalid uid");
    assert!(!invalid_uid.status.success());
    assert!(String::from_utf8(invalid_uid.stderr)
        .expect("diagnostic is UTF-8")
        .contains("invalid SUDO_UID"));

    let missing_gid = launcher()
        .arg("/usr/bin/true")
        .env("SUDO_UID", "501")
        .output()
        .expect("run launcher without gid");
    assert!(!missing_gid.status.success());
    assert!(String::from_utf8(missing_gid.stderr)
        .expect("diagnostic is UTF-8")
        .contains("SUDO_GID is missing"));

    let invalid_gid = launcher()
        .arg("/usr/bin/true")
        .env("SUDO_UID", "501")
        .env("SUDO_GID", "not-a-number")
        .output()
        .expect("run launcher with invalid gid");
    assert!(!invalid_gid.status.success());
    assert!(String::from_utf8(invalid_gid.stderr)
        .expect("diagnostic is UTF-8")
        .contains("invalid SUDO_GID"));
}
