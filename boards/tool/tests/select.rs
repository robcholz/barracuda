//! User-flow tests for persistent Board selection.

#![allow(clippy::expect_used)]

use std::{fs, path::Path};

use barracuda_board_config::{read_selected_board, write_selected_board};
use barracuda_board_tool::{run, CommandError};
use tempfile::tempdir;

fn add_board(root: &Path, directory_name: &str, declared_name: &str) {
    let directory = root.join("boards/configs").join(directory_name);
    fs::create_dir_all(&directory).expect("Board directory");
    fs::write(
        directory.join("board.yml"),
        format!(
            "name: {declared_name}\nhardware:\n  chip: macos\nnative-layout:\n  artifact: file-layout.yml\n"
        ),
    )
    .expect("Board YAML");
    fs::write(
        directory.join("file-layout.yml"),
        "capacity: 1\nregions: []\n",
    )
    .expect("native layout");
}

fn add_cross_board(root: &Path, name: &str, target: &str) {
    let directory = root.join("boards/configs").join(name);
    fs::create_dir_all(&directory).expect("Board directory");
    fs::write(
        directory.join("board.yml"),
        format!(
            "name: {name}\nhardware:\n  chip: cross\ntoolchain:\n  target: {target}\nnative-layout:\n  artifact: layout.bin\n"
        ),
    )
    .expect("Board YAML");
    fs::write(directory.join("layout.bin"), "layout\n").expect("native layout");
}

#[test]
fn select_persists_a_valid_board_for_the_next_build() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let mut output = Vec::new();

    run(["select", "local-macos"], root.path(), &mut output).expect("select Board");

    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        Some(String::from("local-macos"))
    );
    assert_eq!(
        String::from_utf8(output).expect("UTF-8 output"),
        "Selected Board `local-macos`.\nRun `cargo run` to build and start it.\n"
    );
}

#[test]
fn unknown_board_does_not_replace_the_previous_selection() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    write_selected_board(root.path(), "local-macos").expect("initial selection");

    let error =
        run(["select", "missing-board"], root.path(), &mut Vec::new()).expect_err("unknown Board");

    assert!(error
        .to_string()
        .contains("Board `missing-board` does not exist"));
    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        Some(String::from("local-macos"))
    );
}

#[test]
fn mismatched_bundle_name_is_rejected_before_selection_changes() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "product-a", "product-b");

    let error = run(["select", "product-a"], root.path(), &mut Vec::new())
        .expect_err("mismatched Board bundle");

    assert!(error
        .to_string()
        .contains("directory `product-a` declares Board `product-b`"));
    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        None
    );
}

#[test]
fn missing_native_layout_is_rejected() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    fs::remove_file(
        root.path()
            .join("boards/configs/local-macos/file-layout.yml"),
    )
    .expect("remove native layout");

    let error = run(["select", "local-macos"], root.path(), &mut Vec::new())
        .expect_err("incomplete Board bundle");

    assert!(error.to_string().contains("native layout"));
    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        None
    );
}

#[test]
fn command_requires_exact_select_syntax() {
    let root = tempdir().expect("temporary workspace");

    for args in [vec![], vec!["build"], vec!["select", "a", "b"]] {
        let error = run(args, root.path(), &mut Vec::new()).expect_err("invalid arguments");
        assert!(matches!(error, CommandError::Arguments(_)));
    }
}

#[test]
fn target_prints_the_boards_declared_toolchain_triple() {
    let root = tempdir().expect("temporary workspace");
    add_cross_board(
        root.path(),
        "esp32c6-devkitc-1",
        "riscv32imac-unknown-none-elf",
    );
    let mut output = Vec::new();

    run(["target", "esp32c6-devkitc-1"], root.path(), &mut output).expect("read Board target");

    assert_eq!(
        String::from_utf8(output).expect("UTF-8 output"),
        "riscv32imac-unknown-none-elf\n"
    );
}

#[test]
fn target_reads_the_persisted_selection_when_no_board_is_named() {
    let root = tempdir().expect("temporary workspace");
    add_cross_board(root.path(), "stm32f429zi-nucleo", "thumbv7em-none-eabihf");
    write_selected_board(root.path(), "stm32f429zi-nucleo").expect("select Board");
    let mut output = Vec::new();

    run(["target"], root.path(), &mut output).expect("read selected Board target");

    assert_eq!(
        String::from_utf8(output).expect("UTF-8 output"),
        "thumbv7em-none-eabihf\n"
    );
}

#[test]
fn target_of_a_host_board_prints_nothing() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let mut output = Vec::new();

    run(["target", "local-macos"], root.path(), &mut output).expect("read host Board target");

    assert!(String::from_utf8(output).expect("UTF-8 output").is_empty());
}

#[test]
fn target_without_a_selection_reports_no_board() {
    let root = tempdir().expect("temporary workspace");

    let error = run(["target"], root.path(), &mut Vec::new()).expect_err("no selection");

    assert!(error.to_string().contains("no Board selected"));
}

#[test]
fn cargo_config_exposes_board_without_replacing_builtin_build() {
    let config = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.cargo/config.toml"
    ))
    .expect("workspace Cargo config");

    assert!(config.contains("board = \"run --quiet --package barracuda-board-tool --\""));
    assert!(!config
        .lines()
        .any(|line| line.trim_start().starts_with("build =")));
}

#[test]
fn normal_cargo_run_reaches_the_board_selected_build_driver() {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .expect("workspace manifest");
    let default_members = manifest
        .split_once("default-members = [")
        .and_then(|(_before, after)| after.split_once(']'))
        .map(|(members, _after)| members)
        .expect("default members");

    assert!(default_members.contains("tools/barracuda-build"));
    assert!(!default_members.contains("apps/barracuda-cli"));
}

#[test]
fn repository_board_catalog_ids_are_unique() {
    let configs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards/configs");
    let mut ids = Vec::new();
    for entry in fs::read_dir(&configs).expect("read Board catalog") {
        let entry = entry.expect("read Board catalog entry");
        if !entry.file_type().expect("Board entry type").is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .expect("Board directory name is UTF-8");
        ids.push(name);
    }
    ids.sort_unstable();
    assert!(
        !ids.is_empty(),
        "Board catalog under boards/configs must not be empty"
    );

    let mut normalized = std::collections::BTreeMap::<String, String>::new();
    for id in ids {
        let key: String = id
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect();
        assert_eq!(
            normalized.insert(key, id.clone()),
            None,
            "Board catalog ids must stay unique after removing separators; `{id}` collides"
        );
    }
}

#[test]
fn every_board_consumer_reads_the_persisted_selection() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in [
        "boards/selected/build.rs",
        "platforms/esp32c6/build.rs",
        "platforms/linux/build.rs",
        "platforms/macos/build.rs",
        "platforms/stm32/build.rs",
    ] {
        let source = fs::read_to_string(root.join(relative)).expect("Board build script");
        assert!(
            source.contains("read_selected_board"),
            "{relative} bypasses the persistent Board selection"
        );
        assert!(
            source.contains("SELECTED_BOARD_PATH"),
            "{relative} does not tell Cargo to watch the selection"
        );
        assert!(
            !source.contains("BARRACUDA_BOARD"),
            "{relative} still exposes the old environment-variable path"
        );
    }
}
