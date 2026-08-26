//! Architecture boundary for the Agent-owned model transport.

use std::path::Path;

#[test]
fn model_api_has_one_concrete_transport_without_adapter_layers() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(root.join("src/lib.rs"))?;
    let client = std::fs::read_to_string(root.join("src/client.rs"))?;
    let transport = std::fs::read_to_string(root.join("src/transport.rs"))?;
    let backends = std::fs::read_to_string(root.join("src/backends/mod.rs"))?;
    let shared = std::fs::read_to_string(root.join("src/backends/shared.rs"))?;
    let errors = std::fs::read_to_string(root.join("src/errors.rs"))?;
    let retry = std::fs::read_to_string(root.join("src/retry.rs"))?;
    let sse = std::fs::read_to_string(root.join("src/backends/sse.rs"))?;

    assert!(!root.join("src/http").exists());
    assert!(!lib.contains("pub use http::Client as HttpClient"));
    assert!(!client.contains("with_http_client"));
    assert!(!transport.contains("trait TransportBackend"));
    assert!(!transport.contains("dyn TransportBackend"));
    assert!(!transport.contains("struct HttpTransport"));
    assert!(!transport.contains("Mutex<"));
    assert!(transport.contains("http_client::ClientFactory"));
    assert!(transport.contains("post_json"));
    assert!(transport.contains("post_json_stream"));
    assert!(!backends.contains("pub(crate) enum Backend"));
    assert!(!shared.contains("struct BackendContext"));
    assert!(!errors.contains("enum ModelApiError"));
    assert!(!errors.contains("enum ChatError"));
    assert!(!errors.contains("enum ChatJsonError"));
    assert!(!errors.contains("enum InferMediaError"));
    assert!(!retry.contains("trait RetryError"));
    assert!(!lib.contains("struct StatusCode"));
    assert!(!sse.contains("trait SseParse"));
    Ok(())
}
