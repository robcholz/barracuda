//! The selected-target crate must own Board/Platform resource construction.

#[test]
fn application_uses_the_selected_target_resource_factory() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system = std::fs::read_to_string(root.join("core/system/src/lib.rs"))?;
    let target = std::fs::read_to_string(root.join("composition/selected/src/lib.rs"))?;
    let application = std::fs::read_to_string(root.join("apps/barracuda-system/src/lib.rs"))?;
    let application_entry =
        std::fs::read_to_string(root.join("apps/barracuda-system/src/main.rs"))?;
    let platform_entry = std::fs::read_to_string(root.join("platforms/macos/src/application.rs"))?;

    assert!(target.contains("barracuda_platform_selected::prepare()"));
    assert!(target.contains("barracuda_board_selected::resources(spawner, board_bindings)"));
    assert!(target.contains("bindings.split()"));
    assert!(application_entry.contains("barracuda_target::application_entry!(application)"));
    assert!(application_entry.contains("barracuda_target::resources_with_bindings"));
    assert!(application_entry.contains("barracuda_system_app::run"));
    assert!(target.contains("macro_rules! application_entry"));
    assert!(platform_entry.contains("macro_rules! platform_entry"));
    assert!(platform_entry.contains("$application"));
    assert!(!platform_entry.contains("barracuda_target"));
    assert!(!platform_entry.contains("barracuda_system_app"));
    assert!(application.contains("System::new(resources, spawner)"));
    assert!(system.contains("TargetResources<"));
    assert!(system.contains("PlatformResources<Tls, Partitions<"));
    assert!(system.contains("mount_or_format_partition(prepared.partitions.system)"));
    assert!(system.contains("prepared.partitions.resources.filesystem"));
    assert!(system.contains("mod read_only_flash;"));
    let resources_mount = std::fs::read_to_string(root.join("core/system/src/read_only_flash.rs"))?;
    assert!(resources_mount.contains("PartitionFilesystem::FatFs"));
    assert!(resources_mount.contains("PartitionFilesystem::LittleFs"));
    assert!(!resources_mount.contains("detect_resources_filesystem"));
    assert!(!resources_mount.contains("FAT_BOOT_SECTOR"));
    assert!(system.contains("MountOptions::read_only()"));
    assert!(system.contains("mount(\"/resources\", resources, MountOptions::read_only())"));
    assert!(system.contains("BlockingAsync::new(prepared.partitions.kv_database)"));
    assert!(system.contains("mount(\"/data\", backend.clone(), MountOptions::read_write())"));
    assert!(system.contains("create_dir_all(\"/data/media\")"));
    assert!(system
        .contains("mount_scoped(\"/media\", backend, \"/media\", MountOptions::read_write())"));
    assert!(system.contains("MemFs::new().into_backend()"));
    assert!(system.contains("mount(\"/cache\", cache, MountOptions::read_write())"));
    assert!(system.contains("$manager.register_all()"));
    assert!(system.contains("plugins.start()"));
    assert!(system.contains("plugins.install_vfs(global_namespace().await)"));
    assert!(!system.contains("StorageNotConstructed"));
    let resources = std::fs::read_to_string(root.join("core/system/src/resources.rs"))?;
    let manifest = std::fs::read_to_string(root.join("core/system/Cargo.toml"))?;
    assert!(manifest.contains("barracuda-vfs-memfs.workspace = true"));
    assert!(resources.contains("SYSTEM_PARTITION: &str = \"system\""));
    assert!(resources.contains("KV_DATABASE_PARTITION: &str = \"kv_database\""));
    assert!(resources.contains("RESOURCES_PARTITION: &str = \"resources\""));
    assert!(resources.contains("take_partition("));
    assert!(!application.contains("mod selected"));
    assert!(!application.contains("SelectedPlatform::initialize"));
    assert!(!application.contains("target_os"));
    assert!(!application_entry.contains("target_os"));
    assert!(!system.contains("P::initialize(spawner, board)"));
    Ok(())
}

#[test]
fn system_constructs_every_plugin_from_one_public_field_context() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plugin_api = std::fs::read_to_string(root.join("core/plugin/crates/api/src/lib.rs"))?;
    let system = std::fs::read_to_string(root.join("core/system/src/lib.rs"))?;

    assert!(plugin_api.contains("pub struct PluginContext"));
    assert!(plugin_api.contains("pub ip_stack: Stack<'static>"));
    assert!(plugin_api.contains("pub http_clients: ClientFactory<'static>"));
    assert!(system.contains("let mut plugin_context"));
    assert!(system.contains("PluginContext::from_hal("));
    assert!(system.contains("prepared.board_hal"));
    assert!(plugin_api.contains("pub hal: BoardHalResources<Builtins, Arc<Io>>"));
    assert!(!plugin_api.contains("Lua"));
    assert!(!system.contains("Lua"));

    for duplicate_hal in [
        "GpioHardware",
        "I2cHardware",
        "SpiHardware",
        "HardwareFuture",
    ] {
        assert!(!plugin_api.contains(duplicate_hal));
    }

    for plugin in [
        "WorkflowPlugin",
        "WebServerPlugin",
        "VmPlugin",
        "VmFilesystemPlugin",
        "VmHttpPlugin",
        "VmTimePlugin",
        "TimePlugin",
        "SchedulerPlugin",
        "AgentPlugin",
        "IMessageGatewayPlugin",
        "IMessageBlueBubblePlugin",
        "IMessageInkboxPlugin",
        "IMessageQQPlugin",
        "IMessageTelegramPlugin",
        "IMessageWechatPlugin",
        "IMessageWebPlugin",
        "GpioPlugin",
        "I2cPlugin",
        "SpiPlugin",
    ] {
        assert!(
            system.contains(&format!("{plugin}::new(&mut plugin_context)")),
            "{plugin} is not constructed from the shared PluginContext",
        );
    }

    Ok(())
}

#[test]
fn vfs_scopes_replace_the_custom_agent_sandbox_crate() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(!root
        .join("plugins/agent/crates/sandbox/Cargo.toml")
        .exists());

    for relative in [
        "plugins/agent/crates/agent/src",
        "plugins/agent/crates/runtime/src",
        "plugins/agent/crates/tool/src",
    ] {
        let mut files = Vec::new();
        collect_rust_files(&root.join(relative), &mut files)?;
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

#[test]
fn selected_platform_does_not_require_a_system_runtime_feature() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_manifest = std::fs::read_to_string(root.join("core/system/Cargo.toml"))?;
    let application_manifest =
        std::fs::read_to_string(root.join("apps/barracuda-system/Cargo.toml"))?;
    let channel_manifest = std::fs::read_to_string(root.join("apps/barracuda-cli/Cargo.toml"))?;

    assert!(!system_manifest.contains("tokio = ["));
    assert!(!system_manifest.contains("GENERATED PLUGIN FEATURES"));
    assert!(!system_manifest.contains("target_os"));
    assert!(!system_manifest
        .contains("barracuda-webserver-plugin = { workspace = true, features = [\"std\"] }"));
    assert!(application_manifest.contains("barracuda-system.workspace = true"));
    assert!(application_manifest.contains("barracuda-target.workspace = true"));
    assert!(!channel_manifest.contains("barracuda-system.workspace = true"));
    assert!(!channel_manifest.contains("barracuda-target.workspace = true"));
    assert!(!channel_manifest.contains("embassy-executor"));
    Ok(())
}

fn collect_rust_files(
    directory: &std::path::Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}
