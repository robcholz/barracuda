//! The terminal CLI is an external Channel client, never a System owner.

#[test]
fn terminal_channel_has_no_selected_target_or_system_dependencies() {
    let manifest = include_str!("../Cargo.toml");
    let main = include_str!("../src/main.rs");

    for forbidden in [
        "barracuda-system.workspace",
        "barracuda-target.workspace",
        "embassy-executor",
        "mod local_native",
    ] {
        assert!(
            !manifest.contains(forbidden) && !main.contains(forbidden),
            "terminal Channel still owns `{forbidden}`",
        );
    }

    assert!(main.contains("client::run(&url)"));
}

#[test]
fn cargo_cli_always_builds_the_channel_for_the_host() {
    let cargo = include_str!("../../../.cargo/config.toml");

    let alias = cargo
        .lines()
        .find(|line| line.starts_with("cli = "))
        .expect("cargo cli alias");

    assert!(alias.contains(r#""--target", "host-tuple""#));
    assert!(alias.contains(r#""--package", "barracuda-cli""#));
}
