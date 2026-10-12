//! The Agent's files are reached through VFS scopes, not a sandbox crate of
//! its own.

#[test]
fn vfs_scopes_replace_the_custom_agent_sandbox_crate() -> Result<(), std::io::Error> {
    let agent = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(!agent.join("crates/sandbox/Cargo.toml").exists());

    for relative in ["crates/agent/src", "crates/runtime/src", "crates/tool/src"] {
        let mut files = Vec::new();
        collect_rust_files(&agent.join(relative), &mut files)?;
        for path in files {
            let source = std::fs::read_to_string(&path)?;
            for forbidden in ["SandboxFs", "RealRoots", "barracuda_agent_sandbox"] {
                assert!(
                    !source.contains(forbidden),
                    "{} still references obsolete `{forbidden}`",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn collect_rust_files(
    directory: &std::path::Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
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
