//! The terminal CLI is an external Channel client, never a System owner.

#[test]
fn terminal_channel_has_no_selected_target_or_system_dependencies() {
    let manifest = include_str!("../Cargo.toml");
    let main = include_str!("../src/main.rs");

    for forbidden in [
        "barracuda-system.workspace",
        "barracuda-target.workspace",
        "barracuda-event-router.workspace",
        "embassy-executor",
        "mod local_native",
    ] {
        assert!(
            !manifest.contains(forbidden) && !main.contains(forbidden),
            "terminal Channel still owns `{forbidden}`",
        );
    }

    assert!(main.contains("client::run(url)"));
}

#[test]
fn cargo_cli_always_builds_the_channel_for_the_host() {
    let cargo = include_str!("../../../.cargo/config.toml");

    assert!(cargo.contains("cli = \"run --target host-tuple --package barracuda-cli -- connect\""));
}
