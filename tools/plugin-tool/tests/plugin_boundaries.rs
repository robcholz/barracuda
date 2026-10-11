//! Boundaries every Plugin keeps, checked over all Plugins in the workspace.

use std::path::{Path, PathBuf};

/// Plugin storage is semantic: no Plugin names the flash its database lives
/// on.
#[test]
fn no_plugin_names_a_flash_contract() -> Result<(), std::io::Error> {
    let mut files = Vec::new();
    collect_plugin_sources(&workspace_root().join("plugins"), &mut files)?;
    assert!(!files.is_empty(), "no Plugin sources found");

    let mut offenders = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path)?;
        if ["PartitionFlash", "NorFlash"]
            .iter()
            .any(|contract| source.contains(contract))
        {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "Plugins name a flash contract: {}",
        offenders.join(", ")
    );
    Ok(())
}

/// Collects the Rust sources under every `crates/*/src` of a Plugin.
fn collect_plugin_sources(plugins: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for plugin in std::fs::read_dir(plugins)? {
        let crates = plugin?.path().join("crates");
        if !crates.is_dir() {
            continue;
        }
        for krate in std::fs::read_dir(crates)? {
            let source = krate?.path().join("src");
            if source.is_dir() {
                collect_rust_files(&source, files)?;
            }
        }
    }
    Ok(())
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
