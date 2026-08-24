//! Board YAML parsing, validation, and code generation tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use barracuda_board_config::{parse, render_rust, ConfigError};
use std::path::Path;

const VALID: &str = r#"
name: local-host
hardware:
  chip: host
storage:
  filesystem: filesystem
  web-assets: web-assets
  database: database
"#;

#[test]
fn parses_platform_neutral_board_yaml() {
    let board = parse(VALID).expect("valid Board YAML");

    assert_eq!(board.name(), "local-host");
    assert_eq!(board.hardware().chip(), "host");
    assert_eq!(board.storage().filesystem(), "filesystem");
    assert_eq!(board.storage().web_assets(), Some("web-assets"));
    assert_eq!(board.storage().database(), "database");
}

#[test]
fn rejects_multiple_yaml_documents() {
    let yaml = format!("{VALID}\n---\n{VALID}");
    assert_eq!(parse(&yaml).unwrap_err(), ConfigError::DocumentCount(2));
}

#[test]
fn rejects_empty_storage_binding() {
    let yaml = VALID.replace("database: database", "database: ''");
    assert_eq!(
        parse(&yaml).unwrap_err(),
        ConfigError::EmptyStorageBinding("database")
    );
}

#[test]
fn rejects_empty_hardware_chip() {
    let yaml = VALID.replace("chip: host", "chip: ''");
    assert_eq!(parse(&yaml).unwrap_err(), ConfigError::EmptyChip);
}

#[test]
fn rejects_storage_roles_bound_to_the_same_native_region() {
    let yaml = VALID.replace("database: database", "database: filesystem");
    assert_eq!(
        parse(&yaml).unwrap_err(),
        ConfigError::DuplicateStorageBinding {
            first: "filesystem",
            second: "database",
            region: "filesystem".into(),
        }
    );
}

#[test]
fn rejects_web_assets_bound_to_the_database_region() {
    let yaml = VALID.replace("web-assets: web-assets", "web-assets: database");
    assert_eq!(
        parse(&yaml).unwrap_err(),
        ConfigError::DuplicateStorageBinding {
            first: "web-assets",
            second: "database",
            region: "database".into(),
        }
    );
}

#[test]
fn renders_a_static_board_without_a_platform_type() {
    let board = parse(VALID).expect("valid Board YAML");
    let rust = render_rust(&board);

    assert!(rust.contains("pub const BOARD: ::barracuda_board::Board"));
    assert!(rust.contains("Hardware::new(\"host\")"));
    assert!(rust.contains("Storage::new(\"filesystem\", Some(\"web-assets\"), \"database\")"));
    assert!(!rust.contains("barracuda_partition"));
    assert!(!rust.contains("offset"));
    assert!(!rust.contains("Platform"));
}

#[test]
fn repository_local_host_yaml_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../configs/local-host/board.yml");
    let yaml = std::fs::read_to_string(path).expect("read local Host Board YAML");
    let board = parse(&yaml).expect("valid local Host Board YAML");

    assert_eq!(board.name(), "local-host");
}

#[test]
fn every_repository_board_bundle_has_valid_platform_neutral_yaml() {
    let configs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../configs");
    for entry in std::fs::read_dir(configs).expect("read Board bundles") {
        let path = entry.expect("read Board bundle entry").path();
        if !path.is_dir() {
            continue;
        }
        let yaml = std::fs::read_to_string(path.join("board.yml")).expect("read Board YAML");
        let board = parse(&yaml).expect("valid Board YAML");
        assert_eq!(
            board.name(),
            path.file_name().and_then(|name| name.to_str()).unwrap()
        );
    }
}
