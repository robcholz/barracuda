//! Event Router is the only public surface for its implementation crates.

#[test]
fn workspace_consumers_depend_only_on_the_event_router_facade() -> Result<(), std::io::Error> {
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
            ("barracuda-router", "barracuda_router::"),
            ("barracuda-rpc", "barracuda_rpc::"),
            ("barracuda-workflow", "barracuda_workflow::"),
        ] {
            assert!(
                !contents.contains(package),
                "{} depends directly on the internal {package} crate",
                path.display()
            );
            assert!(
                !contents.contains(module),
                "{} uses the internal {package} crate instead of the Event Router facade",
                path.display()
            );
        }
    }

    Ok(())
}

#[test]
fn implementation_crates_are_not_publishable() {
    for manifest in [
        include_str!("../crates/router/Cargo.toml"),
        include_str!("../crates/rpc/Cargo.toml"),
        include_str!("../crates/workflow/Cargo.toml"),
    ] {
        assert!(
            manifest.contains("publish = false"),
            "Event Router implementation crates must not be published directly"
        );
    }
}

fn collect_sources_and_manifests(
    directory: &std::path::Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
    if !directory.exists() {
        return Ok(());
    }
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
