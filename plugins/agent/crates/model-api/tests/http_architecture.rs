//! Architecture boundary for the Agent-owned HTTP transport.

use std::path::Path;

#[test]
fn model_api_has_no_generic_http_facade() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(root.join("src/lib.rs"))?;
    let client = std::fs::read_to_string(root.join("src/client.rs"))?;
    let transport = std::fs::read_to_string(root.join("src/transport.rs"))?;

    assert!(!root.join("src/http").exists());
    assert!(!lib.contains("pub use http::Client as HttpClient"));
    assert!(!client.contains("with_http_client"));
    assert!(!transport.contains("dyn Backend"));
    assert!(!transport.contains("Mutex<"));
    assert!(transport.contains("http_client::ClientFactory"));
    assert!(transport.contains("post_json"));
    assert!(transport.contains("post_json_stream"));
    Ok(())
}
