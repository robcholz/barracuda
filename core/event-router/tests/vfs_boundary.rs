//! Event Router persistence uses the real System VFS and owns one exact file.

#[test]
fn event_router_no_longer_depends_on_the_legacy_filesystem_crate() {
    let manifest = include_str!("../Cargo.toml");
    let library = include_str!("../src/lib.rs");
    let workflow = include_str!("../src/workflow.rs");

    assert!(!manifest.contains("barracuda-fs"));
    assert!(manifest.contains("barracuda-vfs"));
    assert!(!library.contains("FileSystem"));
    assert!(!workflow.contains("FileSystem"));
    assert!(!library.contains("filesystem: Vfs"));
    assert!(!workflow.contains("filesystem: Vfs"));
    assert!(!workflow.contains("&Vfs"));
    assert!(!workflow.contains("ScopedVfs"));
    assert!(workflow.contains("read(WORKFLOW_CATALOG_PATH)"));
    assert!(workflow.contains("/system/workflows.json"));
    assert!(!workflow.contains("WORKFLOW_INDEX_FILE"));
    assert!(!workflow.contains("workflow_path"));
}
