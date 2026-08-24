//! Workspace boundary checks for the single HTTP implementation policy.

use std::path::{Path, PathBuf};

#[test]
fn reqwless_is_owned_only_by_shared_http_client() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let owner = root.join("shared/http-client");
    let mut files = Vec::new();
    collect_source_and_manifests(&root, &owner, &mut files)?;

    for path in files {
        let source = std::fs::read_to_string(&path)?;
        let leaks_implementation = match path.file_name().and_then(|name| name.to_str()) {
            Some("Cargo.toml") => source.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("reqwless =") || line.starts_with("reqwless.")
            }),
            _ => source.contains("use reqwless::") || source.contains("reqwless::client::"),
        };
        assert!(
            !leaks_implementation,
            "{} bypasses shared/http-client and imports reqwless directly",
            path.display()
        );
    }
    Ok(())
}

fn collect_source_and_manifests(
    directory: &Path,
    owner: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if directory == owner || directory.ends_with("target") || directory.ends_with(".git") {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_source_and_manifests(&path, owner, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
    Ok(())
}
