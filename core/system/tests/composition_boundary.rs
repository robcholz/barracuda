//! The selected-target crate must own Board/Platform resource construction.

#[test]
fn application_uses_the_selected_target_resource_factory() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system = std::fs::read_to_string(root.join("core/system/src/lib.rs"))?;
    let target = std::fs::read_to_string(root.join("platforms/selected/src/lib.rs"))?;
    let application = std::fs::read_to_string(root.join("apps/barracuda-cli/src/local_host.rs"))?;

    assert!(target.contains("SelectedPlatform::prepare()"));
    assert!(target.contains("SelectedPlatform::initialize(spawner, &BOARD)"));
    assert!(application.contains("barracuda_target::resources(spawner)"));
    assert!(!application.contains("mod selected"));
    assert!(!application.contains("SelectedPlatform::initialize"));
    assert!(!system.contains("P::initialize(spawner, board)"));
    Ok(())
}
