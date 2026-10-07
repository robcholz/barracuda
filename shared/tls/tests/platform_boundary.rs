//! TLS sits above the Platform boundary: built by System from Platform entropy.

use std::path::{Path, PathBuf};

#[test]
fn shared_tls_reads_no_operating_system_trust_store() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("src/lib.rs"))?;

    for forbidden in [
        "from_system_certificates",
        "SSL_CERT_FILE",
        "/etc/ssl",
        "/opt/homebrew",
        "std::fs",
        "SysRng",
    ] {
        assert!(
            !source.contains(forbidden),
            "shared TLS reads an operating-system trust source: `{forbidden}`"
        );
    }
    Ok(())
}

#[test]
fn no_platform_initializes_tls() -> Result<(), std::io::Error> {
    let platforms = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms");
    let mut files = Vec::new();
    collect_rust_files(&platforms, &mut files)?;
    for path in files {
        let source = std::fs::read_to_string(&path)?;
        for forbidden in ["barracuda_tls", "SSL_CERT_FILE", "type Tls"] {
            assert!(
                !source.contains(forbidden),
                "{} still owns TLS: `{forbidden}`",
                path.display()
            );
        }
    }
    Ok(())
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
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
