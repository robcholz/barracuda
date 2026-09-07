//! The workspace exposes Plugins, while implementation crates remain private.

use std::path::{Path, PathBuf};

#[test]
fn workspace_catalog_exposes_only_plugin_packages() -> Result<(), std::io::Error> {
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let offenders = manifest
        .lines()
        .filter_map(plugin_path)
        .filter(|path| !path.ends_with("/crates/plugin"))
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "workspace dependencies expose Plugin implementation crates: {}",
        offenders.join(", ")
    );
    Ok(())
}

#[test]
fn workspace_catalog_exposes_only_core_facades() -> Result<(), std::io::Error> {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))?;

    for package in [
        "barracuda-plugin-api",
        "barracuda-plugin-macros",
        "barracuda-plugin-manager",
        "barracuda-plugin-manifest",
    ] {
        assert!(
            !has_workspace_dependency(&manifest, package),
            "workspace dependencies expose internal package {package}"
        );
    }

    assert!(
        has_workspace_dependency(&manifest, "barracuda-plugin"),
        "workspace dependencies do not expose facade barracuda-plugin"
    );

    Ok(())
}

#[test]
fn plugin_implementation_crates_are_not_publishable() -> Result<(), std::io::Error> {
    let root = workspace_root();
    let mut manifests = Vec::new();
    collect_manifests(&root.join("plugins"), &mut manifests)?;

    let offenders = unpublishable_implementation_crates(&root, manifests)?;

    assert!(
        offenders.is_empty(),
        "Plugin implementation crates must set `publish = false`: {}",
        offenders.join(", ")
    );
    Ok(())
}

#[test]
fn manifest_read_errors_are_reported() {
    let root = workspace_root();
    let missing = root.join("plugins/missing/crates/internal/Cargo.toml");

    let result = unpublishable_implementation_crates(&root, vec![missing]);

    assert!(matches!(
        result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ));
}

fn unpublishable_implementation_crates(
    root: &Path,
    manifests: Vec<PathBuf>,
) -> Result<Vec<String>, std::io::Error> {
    let mut offenders = Vec::new();
    for manifest in manifests
        .into_iter()
        .filter(|manifest| !manifest.ends_with("crates/plugin/Cargo.toml"))
    {
        let content = std::fs::read_to_string(&manifest)?;
        if !content.lines().any(|line| line.trim() == "publish = false") {
            offenders.push(
                manifest
                    .strip_prefix(root)
                    .unwrap_or(&manifest)
                    .display()
                    .to_string(),
            );
        }
    }
    Ok(offenders)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn plugin_path(line: &str) -> Option<&str> {
    let path = line.split_once("path = \"")?.1.split_once('"')?.0;
    path.starts_with("plugins/").then_some(path)
}

fn has_workspace_dependency(manifest: &str, package: &str) -> bool {
    manifest.lines().any(|line| {
        line.split_once('=')
            .is_some_and(|(name, _value)| name.trim() == package)
    })
}

fn collect_manifests(directory: &Path, manifests: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_manifests(&path, manifests)?;
        } else if entry.file_name() == "Cargo.toml" {
            manifests.push(path);
        }
    }
    Ok(())
}
