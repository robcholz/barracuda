//! Guards the Browser Platform's boundary from application-specific assumptions.

#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::Path};

#[test]
fn browser_bridge_does_not_name_application_services() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/ffi.rs",
        "src/network.rs",
        "web/bootstrap.js",
        "web/browser_host.js",
        "web/browser_websocket.js",
        "web/index.html",
        "web/service-worker.js",
        "web/worker.js",
    ] {
        let source = fs::read_to_string(root.join(relative))?;
        for forbidden in ["portal", "imessage", "8787"] {
            assert!(
                !source.to_ascii_lowercase().contains(forbidden),
                "Browser Platform source {relative} contains application-specific token `{forbidden}`",
            );
        }
    }
    Ok(())
}

#[test]
fn browser_platform_projects_board_layout_without_board_runtime_bindings(
) -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let platform = fs::read_to_string(root.join("src/lib.rs"))?;
    let build = fs::read_to_string(root.join("build.rs"))?;

    assert!(platform.contains("type Bindings = ();"));
    assert!(!platform.contains("barracuda_board::Board"));
    assert!(!platform.contains("const FLASH_CAPACITY"));
    assert!(!platform.contains("RESOURCES_OFFSET"));
    assert!(build.contains("board.native_layout().artifact()"));
    assert!(platform.contains("for region in regions"));
    Ok(())
}
