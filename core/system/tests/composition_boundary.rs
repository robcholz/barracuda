//! The selected-target crate must own Board/Platform resource construction.

#[test]
fn application_uses_the_selected_target_resource_factory() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system = std::fs::read_to_string(root.join("core/system/src/lib.rs"))?;
    let target = std::fs::read_to_string(root.join("composition/selected/src/lib.rs"))?;
    let application = std::fs::read_to_string(root.join("apps/barracuda-cli/src/local_native.rs"))?;

    assert!(target.contains("barracuda_platform_selected::prepare()"));
    assert!(target.contains("barracuda_board_selected::resources(spawner, board_bindings)"));
    assert!(target.contains("bindings.split()"));
    assert!(application.contains("barracuda_target::resources(spawner)"));
    assert!(application.contains("System::new(lanes(), target_resources(spawner).await?, spawner)"));
    assert!(application.contains(".shutdown()"));
    assert!(system.contains("TargetResources<"));
    assert!(system.contains("PlatformResources<Tls, Partitions<"));
    assert!(system.contains("mount_or_format_partition(prepared.partitions.system)"));
    assert!(system.contains("BlockingAsync::new(prepared.partitions.kv_database)"));
    assert!(system.contains("mount(\"/\", backend, MountOptions::read_write())"));
    assert!(system.contains("EventRouter::new(lanes)"));
    assert!(!system.contains("filesystem.scoped(\"/system/event-router\")"));
    assert!(system.contains("plugins.install_vfs(global_namespace().await)"));
    assert!(!system.contains("StorageNotConstructed"));
    let resources = std::fs::read_to_string(root.join("core/system/src/resources.rs"))?;
    assert!(resources.contains("SYSTEM_PARTITION: &str = \"system\""));
    assert!(resources.contains("KV_DATABASE_PARTITION: &str = \"kv_database\""));
    assert!(resources.contains("WEB_ASSETS_PARTITION: &str = \"web_assets\""));
    assert!(resources.contains("take_partition("));
    assert!(!application.contains("mod selected"));
    assert!(!application.contains("SelectedPlatform::initialize"));
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
    assert!(plugin_api.contains("pub hal: BoardHalResources<Builtins, Io>"));
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
        "FilePlugin",
        "WebServerPlugin",
        "VmPlugin",
        "TimePlugin",
        "SchedulerPlugin",
        "AgentPlugin",
        "CaptivePortalPlugin",
        "IMessageGatewayPlugin",
        "IMessageBlueBubblePlugin",
        "IMessageInkboxPlugin",
        "IMessageQQPlugin",
        "IMessageTelegramPlugin",
        "IMessageWechatPlugin",
        "IMessageWebPlugin",
        "GatewayAgentPlugin",
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
