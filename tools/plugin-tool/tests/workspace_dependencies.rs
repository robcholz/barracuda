//! Workspace members inherit internal dependencies from the root catalog.

use std::path::{Path, PathBuf};

#[test]
fn member_manifests_do_not_repeat_internal_paths() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifests = Vec::new();
    collect_manifests(&root, &mut manifests)?;

    let mut offenders = Vec::new();
    for manifest in manifests {
        if manifest == root.join("Cargo.toml") {
            continue;
        }
        let mut dependency_section = false;
        for (index, line) in std::fs::read_to_string(&manifest)?.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                dependency_section = trimmed.ends_with("dependencies]");
            }
            if dependency_section && line.contains("path =") {
                offenders.push(format!(
                    "{}:{}",
                    manifest.strip_prefix(&root).unwrap_or(&manifest).display(),
                    index + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "member manifests must use `workspace = true`: {}",
        offenders.join(", ")
    );
    Ok(())
}

fn collect_manifests(directory: &Path, manifests: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if name != ".git" && name != "target" {
                collect_manifests(&path, manifests)?;
            }
        } else if entry.file_name() == "Cargo.toml" {
            manifests.push(path);
        }
    }
    Ok(())
}
