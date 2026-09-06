//! The Plugin facade exposes each implementation crate through a namespace.

#[allow(unused_imports)]
use barracuda_plugin::macros::plugin;
#[cfg(feature = "manifest")]
use barracuda_plugin::manifest;
use barracuda_plugin::{api, manager};

#[test]
fn runtime_namespaces_are_public() {
    let _: Option<api::PluginContext> = None;
    let _: Option<manager::PluginId> = None;
}

#[test]
fn implementation_crates_are_not_publishable() {
    for manifest in [
        include_str!("../crates/api/Cargo.toml"),
        include_str!("../crates/macros/Cargo.toml"),
        include_str!("../crates/manager/Cargo.toml"),
        include_str!("../crates/manifest/Cargo.toml"),
    ] {
        assert!(
            manifest.contains("publish = false"),
            "Plugin implementation crates must not be published directly"
        );
    }
}

#[cfg(feature = "manifest")]
#[test]
fn manifest_namespace_is_public_for_host_tools() {
    let manifest =
        manifest::parse("id = \"example\"\ndepends-on = []\ndescription = \"Example Plugin\"\n")
            .expect("parse manifest through facade");

    assert_eq!(manifest.id(), "example");
}

#[test]
fn workspace_consumers_depend_only_on_the_plugin_facade() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();

    for relative in [
        "apps",
        "bench",
        "boards",
        "composition",
        "core/system",
        "drivers",
        "platforms",
        "plugins",
        "shared",
        "tools",
    ] {
        collect_sources_and_manifests(&root.join(relative), &mut files)?;
    }

    for path in files {
        let contents = std::fs::read_to_string(&path)?;
        for (package, module) in [
            ("barracuda-plugin-api", "barracuda_plugin_api::"),
            ("barracuda-plugin-macros", "barracuda_plugin_macros::"),
            ("barracuda-plugin-manager", "barracuda_plugin_manager::"),
            ("barracuda-plugin-manifest", "barracuda_plugin_manifest::"),
        ] {
            if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                assert!(
                    !contents.lines().any(|line| line.starts_with(package)),
                    "{} depends directly on internal package {package}",
                    path.display()
                );
            } else {
                assert!(
                    !contents.contains(module),
                    "{} uses internal package {package} instead of the Plugin facade",
                    path.display()
                );
            }
        }
    }

    Ok(())
}

fn collect_sources_and_manifests(
    directory: &std::path::Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_sources_and_manifests(&path, files)?;
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml")
            || path.extension().is_some_and(|extension| extension == "rs")
        {
            files.push(path);
        }
    }
    Ok(())
}
