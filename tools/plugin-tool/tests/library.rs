#![allow(missing_docs)]

use std::fs;
use std::path::Path;

use barracuda_plugin_tool::{info, sync_with_report, CommandError, SyncStatus};
use tempfile::TempDir;

fn create_system(root: &Path, manifest: &str, source: &str) {
    fs::create_dir_all(root.join("core/system/src")).expect("create System source directory");
    fs::write(root.join("core/system/Cargo.toml"), manifest).expect("write System manifest");
    fs::write(root.join("core/system/src/lib.rs"), source).expect("write System source");
}

fn generated_system(root: &Path) {
    create_system(
        root,
        "[features]\nstd = [\n# BEGIN GENERATED PLUGIN FEATURES\nold\n# END GENERATED PLUGIN FEATURES\n]\n\
         # BEGIN GENERATED PLUGINS\nold\n# END GENERATED PLUGINS\n",
        "fn register() {\n    // BEGIN GENERATED PLUGINS\n    old();\n    // END GENERATED PLUGINS\n}\n",
    );
}

fn add_plugin(
    root: &Path,
    directory: &str,
    id: &str,
    dependencies: &[&str],
    package: &str,
    entry: &str,
    has_std: bool,
) {
    let plugin = root.join("plugins").join(directory).join("crates/plugin");
    fs::create_dir_all(plugin.join("src")).expect("create Plugin source directory");
    let features = if has_std {
        "\n[features]\nstd = []\n"
    } else {
        ""
    };
    fs::write(
        plugin.join("Cargo.toml"),
        format!("[package]\nname = \"{package}\"\n{features}"),
    )
    .expect("write Plugin Cargo manifest");
    fs::write(
        root.join("plugins").join(directory).join("plugin.toml"),
        format!(
            "id = \"{id}\"\ndepends-on = [{}]\ndescription = \"{directory} behavior.\"\n",
            dependencies
                .iter()
                .map(|dependency| format!("\"{dependency}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .expect("write Plugin metadata");
    fs::write(
        plugin.join("src/lib.rs"),
        format!("impl<const M: usize> Plugin<M> for {entry} {{}}\n"),
    )
    .expect("write Plugin source");
}

fn workspace() -> TempDir {
    let root = tempfile::tempdir().expect("create workspace");
    fs::create_dir(root.path().join("plugins")).expect("create Plugins directory");
    generated_system(root.path());
    root
}

#[test]
fn synchronization_detects_staleness_then_renders_complete_registry_metadata() {
    let root = workspace();
    add_plugin(
        root.path(),
        "base",
        "base-id",
        &[],
        "barracuda-base-plugin",
        "BasePlugin",
        true,
    );
    add_plugin(
        root.path(),
        "consumer",
        "consumer-id",
        &["base-id"],
        "barracuda-consumer-plugin",
        "ConsumerPlugin",
        false,
    );

    assert!(matches!(
        sync_with_report(root.path(), true),
        Err(CommandError::Stale)
    ));
    let updated = sync_with_report(root.path(), false).expect("synchronize stale registry");
    assert_eq!(updated.status(), SyncStatus::Updated);
    assert_eq!(updated.enabled(), ["base", "consumer"]);
    assert!(updated.disabled().is_empty());

    let current = sync_with_report(root.path(), true).expect("validate current registry");
    assert_eq!(current.status(), SyncStatus::Current);
    let manifest = fs::read_to_string(root.path().join("core/system/Cargo.toml"))
        .expect("read generated manifest");
    assert!(manifest
        .contains("barracuda-base-plugin = { path = \"../../plugins/base/crates/plugin\" }"));
    assert!(manifest.contains("\"barracuda-base-plugin/std\","));
    assert!(!manifest.contains("\"barracuda-consumer-plugin/std\","));
    let source = fs::read_to_string(root.path().join("core/system/src/lib.rs"))
        .expect("read generated source");
    assert!(source.contains("barracuda_base_plugin::BasePlugin::new(&mut plugin_context)"));
    assert!(source.contains("barracuda_consumer_plugin::ConsumerPlugin::new(&mut plugin_context)"));

    let base = info(root.path(), "base-id").expect("look up Plugin by identity");
    assert_eq!(base.directory(), "base");
    assert_eq!(base.dependents(), ["consumer-id"]);
    assert!(base.enabled());
    let consumer = info(root.path(), "consumer").expect("look up Plugin by directory");
    assert_eq!(consumer.dependencies(), ["base-id"]);
    assert_eq!(consumer.description(), "consumer behavior.");
}

#[test]
fn discovery_ignores_non_plugin_entries_and_normalizes_disabled_state() {
    let root = workspace();
    add_plugin(
        root.path(),
        "demo",
        "demo-id",
        &[],
        "barracuda-demo-plugin",
        "DemoPlugin",
        false,
    );
    fs::write(root.path().join("plugins/README.md"), "not a Plugin").expect("write plain file");
    fs::create_dir(root.path().join("plugins/not-a-plugin")).expect("create ordinary directory");
    fs::create_dir_all(root.path().join(".barracuda")).expect("create selection directory");
    fs::write(
        root.path().join(".barracuda/disabled-plugins"),
        "\n demo \ndemo\n",
    )
    .expect("write duplicate disabled state");

    let report = sync_with_report(root.path(), false).expect("synchronize selected registry");
    assert!(report.enabled().is_empty());
    assert_eq!(report.disabled(), ["demo"]);
    assert!(!info(root.path(), "demo-id")
        .expect("discover Plugin")
        .enabled());
    assert!(matches!(
        info(root.path(), "missing"),
        Err(CommandError::NotFound(name)) if name == "missing"
    ));
}

#[test]
fn discovery_reports_actionable_manifest_and_entrypoint_failures() {
    let cases = [
        ("no-package", "[workspace]\n", "impl<const M: usize> Plugin<M> for DemoPlugin {}\n", "missing package name"),
        ("no-entry", "[package]\nname = \"demo\"\n", "pub struct DemoPlugin;\n", "expected one Plugin implementation"),
        (
            "ambiguous-entry",
            "[package]\nname = \"demo\"\n",
            "impl<const M: usize> Plugin<M> for FirstPlugin {}\nimpl<const M: usize> Plugin<M> for SecondPlugin {}\n",
            "expected one Plugin implementation",
        ),
    ];

    for (directory, cargo_manifest, source, expected) in cases {
        let root = workspace();
        let plugin = root
            .path()
            .join("plugins")
            .join(directory)
            .join("crates/plugin");
        fs::create_dir_all(plugin.join("src")).expect("create malformed Plugin");
        fs::write(plugin.join("Cargo.toml"), cargo_manifest).expect("write Cargo manifest");
        fs::write(plugin.join("src/lib.rs"), source).expect("write Plugin source");
        fs::write(
            root.path()
                .join("plugins")
                .join(directory)
                .join("plugin.toml"),
            format!(
                "id = \"{directory}\"\ndepends-on = []\ndescription = \"Malformed fixture.\"\n"
            ),
        )
        .expect("write Plugin metadata");

        let error = sync_with_report(root.path(), false).expect_err("discovery must fail");
        assert!(
            error.to_string().contains(expected),
            "unexpected error: {error}"
        );
    }

    let root = tempfile::tempdir().expect("create missing workspace");
    assert!(matches!(
        sync_with_report(root.path(), false),
        Err(CommandError::Io { path, .. }) if path == root.path().join("plugins")
    ));
}

#[test]
fn synchronization_rejects_each_malformed_generated_block_boundary() {
    let cases = [
        (
            "[features]\n# BEGIN GENERATED PLUGIN FEATURES\n# END GENERATED PLUGIN FEATURES\n\
             # BEGIN GENERATED PLUGINS\n# END GENERATED PLUGINS\n",
            "fn register() {}\n",
            "missing generated block marker `// BEGIN GENERATED PLUGINS`",
        ),
        (
            "[features]\n# BEGIN GENERATED PLUGIN FEATURES\n# END GENERATED PLUGIN FEATURES\n# BEGIN GENERATED PLUGINS\n",
            "// BEGIN GENERATED PLUGINS\n// END GENERATED PLUGINS\n",
            "missing generated block marker `# END GENERATED PLUGINS`",
        ),
        (
            "prefix # BEGIN GENERATED PLUGINS\n# END GENERATED PLUGINS\n# BEGIN GENERATED PLUGIN FEATURES\n# END GENERATED PLUGIN FEATURES\n",
            "// BEGIN GENERATED PLUGINS\n// END GENERATED PLUGINS\n",
            "invalid indentation before `# BEGIN GENERATED PLUGINS`",
        ),
    ];

    for (manifest, source, expected) in cases {
        let root = workspace();
        create_system(root.path(), manifest, source);
        let error = sync_with_report(root.path(), false).expect_err("malformed block must fail");
        assert_eq!(error.to_string(), expected);
    }
}
