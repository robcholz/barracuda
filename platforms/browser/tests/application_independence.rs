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
