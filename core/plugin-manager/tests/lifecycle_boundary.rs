//! Architectural boundary tests for Plugin lifecycle phases.

#![allow(clippy::expect_used)]

#[test]
fn component_loading_is_explicitly_registration_only() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lifecycle.rs"),
    )
    .expect("read Plugin lifecycle source");

    assert!(source.contains("pub struct PluginEventRouterContext"));
    assert!(source.contains("pub event_router: PluginEventRouterContext"));
    assert!(source.contains("pub struct PluginRegisterContext"));
    assert!(!source.contains("pub struct PluginContext"));
    assert!(source.contains("pub struct PluginStartContext"));

    let start_context = source
        .split("pub struct PluginStartContext")
        .nth(1)
        .expect("PluginStartContext declaration")
        .split("impl<Storage")
        .next()
        .expect("PluginStartContext fields");
    assert!(!start_context.contains("registrar"));
    assert!(!start_context.contains("component_ids"));
    assert!(!start_context.contains("event_router"));
}

#[test]
fn plugin_start_uses_the_hook_only_context() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lifecycle.rs"),
    )
    .expect("read Plugin lifecycle source");

    let plugin_trait = source
        .split("pub trait Plugin")
        .nth(1)
        .expect("Plugin trait")
        .split("trait ManagedPlugin")
        .next()
        .expect("Plugin trait body");
    assert!(plugin_trait.contains("PluginStartContext"));
    assert!(!plugin_trait.contains("context.load"));
}

#[test]
fn embassy_task_spawner_is_available_only_during_startup() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lifecycle.rs"),
    )
    .expect("read Plugin lifecycle source");

    let register_context = source
        .split("pub struct PluginRegisterContext")
        .nth(1)
        .expect("PluginRegisterContext declaration")
        .split("impl<const M")
        .next()
        .expect("PluginRegisterContext fields");
    assert!(!register_context.contains("task_spawner"));

    let start_context = source
        .split("pub struct PluginStartContext")
        .nth(1)
        .expect("PluginStartContext declaration")
        .split("impl<Storage")
        .next()
        .expect("PluginStartContext fields");
    assert!(start_context.contains("task_spawner"));
}

#[test]
fn plugin_registration_and_start_hooks_are_synchronous() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lifecycle.rs"),
    )
    .expect("read Plugin lifecycle source");

    assert!(!source.contains("pub type PluginRegisterFuture"));
    assert!(!source.contains("pub type PluginStartFuture"));
    assert!(!source.contains("pub async fn register"));
    assert!(!source.contains("pub async fn start"));
    assert!(source.contains("pub async fn unload"));

    let plugin_trait = source
        .split("pub trait Plugin")
        .nth(1)
        .expect("Plugin trait")
        .split("trait ManagedPlugin")
        .next()
        .expect("Plugin trait body");
    assert!(!plugin_trait.contains("Box::pin"));
    assert!(plugin_trait.contains("PluginResult<()>"));
}

#[test]
fn system_resources_are_not_runtime_plugin_capabilities() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lifecycle.rs"),
    )
    .expect("read Plugin lifecycle source");

    for forbidden in [
        "SystemCapabilityRegistry",
        "system_capabilities",
        "provide_system",
        "require_system",
        "SystemNotProvided",
        "SystemAlreadyProvided",
        "SystemTypeMismatch",
    ] {
        assert!(
            !source.contains(forbidden),
            "System resource lookup leaked into Plugin Manager through {forbidden}",
        );
    }
}
