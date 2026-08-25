//! Board YAML parsing, validation, and code generation tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use barracuda_board_config::{parse, render_rust, ConfigError};
use std::path::Path;

const VALID: &str = r#"
name: local-macos
hardware:
  chip: macos
native-layout:
  artifact: file-layout.yml
"#;

#[test]
fn parses_concrete_board_hardware_and_native_layout() {
    let board = parse(VALID).expect("valid Board YAML");

    assert_eq!(board.name(), "local-macos");
    assert_eq!(board.hardware().chip(), "macos");
    assert_eq!(board.native_layout().artifact(), "file-layout.yml");
}

#[test]
fn rejects_multiple_yaml_documents() {
    let yaml = format!("{VALID}\n---\n{VALID}");
    assert_eq!(parse(&yaml).unwrap_err(), ConfigError::DocumentCount(2));
}

#[test]
fn rejects_empty_hardware_chip() {
    let yaml = VALID.replace("chip: macos", "chip: ''");
    assert_eq!(parse(&yaml).unwrap_err(), ConfigError::EmptyChip);
}

#[test]
fn rejects_empty_native_layout_artifact() {
    let yaml = VALID.replace("file-layout.yml", "''");
    assert_eq!(
        parse(&yaml).unwrap_err(),
        ConfigError::EmptyNativeLayoutArtifact
    );
}

#[test]
fn rejects_native_layout_paths_that_escape_the_board_bundle() {
    let yaml = VALID.replace("file-layout.yml", "../file-layout.yml");
    assert_eq!(
        parse(&yaml).unwrap_err(),
        ConfigError::InvalidNativeLayoutArtifact
    );
}

#[test]
fn renders_a_static_board_without_platform_or_system_types() {
    let board = parse(VALID).expect("valid Board YAML");
    let rust = render_rust(&board);

    assert!(rust.contains("pub const BOARD: ::barracuda_board::Board"));
    assert!(rust.contains("Hardware::new(\"macos\")"));
    assert!(rust.contains("NativeLayout::new(\"file-layout.yml\")"));
    assert!(!rust.contains("Storage::new"));
    assert!(!rust.contains("Platform"));
}

#[test]
fn every_repository_board_bundle_has_valid_yaml_and_native_layout() {
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
        assert!(path.join(board.native_layout().artifact()).is_file());
    }
}
