//! Platform-specific certificate discovery must stay in concrete Platforms.

#[test]
fn shared_tls_contains_only_tls_mechanisms() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
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
            "shared TLS still owns Platform-specific `{forbidden}`"
        );
    }
    Ok(())
}
