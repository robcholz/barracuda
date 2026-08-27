//! Workspace boundary checks for the single HTTP implementation policy.

use std::path::{Path, PathBuf};

#[test]
fn shared_http_exposes_a_client_factory() -> Result<(), std::io::Error> {
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))?;

    assert!(source.contains("pub struct ClientFactory"));
    assert!(source.contains("pub fn create("));
    assert!(!source.contains("pub struct Http"));
    Ok(())
}

#[test]
fn reqwless_clients_are_constructed_only_by_shared_http_client() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let owner = root.join("shared/http-client");
    let mut files = Vec::new();
    collect_source_and_manifests(&root, &owner, &mut files)?;

    for path in files {
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        for constructor in [
            "ReqwlessHttpClient::new(",
            "ReqwlessHttpClient::new_with_tls(",
            "reqwless::client::HttpClient::new(",
            "reqwless::client::HttpClient::new_with_tls(",
        ] {
            assert!(
                !source.contains(constructor),
                "{} constructs a reqwless client outside shared/http-client",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn reqwless_is_owned_only_by_transport_crates() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let owner = root.join("shared/http-client");
    let agent_transport = root.join("plugins/agent/crates/model-api");
    let gateway_transport = root.join("plugins/imessage-gateway/crates/http");
    let mut files = Vec::new();
    collect_source_and_manifests(&root, &owner, &mut files)?;

    for path in files {
        if path.starts_with(&agent_transport) || path.starts_with(&gateway_transport) {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        let leaks_implementation = match path.file_name().and_then(|name| name.to_str()) {
            Some("Cargo.toml") => source.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("reqwless =") || line.starts_with("reqwless.")
            }),
            _ => source.contains("use reqwless::") || source.contains("reqwless::client::"),
        };
        assert!(
            !leaks_implementation,
            "{} imports reqwless outside an owning transport crate",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn workspace_contains_no_removed_http_backend_trait() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let owner = root.join("shared/http-client");
    let mut files = Vec::new();
    collect_source_and_manifests(&root, &owner, &mut files)?;

    for path in files {
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        assert!(
            !source.contains("dyn HttpClient")
                && !source.contains("impl HttpClient")
                && !source.contains("http_client::HttpClient"),
            "{} depends on the removed shared HTTP backend trait",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn shared_http_client_contains_no_agent_transport_policy() -> Result<(), std::io::Error> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    for dependency in [
        "barracuda-runtime-utils",
        "embassy-sync",
        "futures-core",
        "futures-lite",
        "ouroboros",
    ] {
        assert!(
            !manifest.contains(dependency),
            "shared/http-client must not own Agent transport dependency {dependency}"
        );
    }

    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    for policy_type in [
        "Body",
        "Backend",
        "HttpFuture",
        "RequestBuilder",
        "ResponsePart",
        "ResponseStream",
    ] {
        assert!(
            !source.contains(policy_type),
            "shared/http-client must not expose Agent transport type {policy_type}"
        );
    }
    Ok(())
}

fn collect_source_and_manifests(
    directory: &Path,
    owner: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if directory == owner || directory.ends_with("target") || directory.ends_with(".git") {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_source_and_manifests(&path, owner, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
    Ok(())
}
