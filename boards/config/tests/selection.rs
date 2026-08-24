//! Persistent Board selection state tests.

#![allow(clippy::expect_used)]

use std::fs;

use barracuda_board_config::{read_selected_board, write_selected_board, SelectionError};
use tempfile::tempdir;

#[test]
fn missing_selection_is_reported_as_unselected() {
    let root = tempdir().expect("temporary workspace");

    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        None
    );
}

#[test]
fn selection_round_trips_through_workspace_state() {
    let root = tempdir().expect("temporary workspace");

    write_selected_board(root.path(), "esp32c6-devkitc-1").expect("write selection");

    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        Some(String::from("esp32c6-devkitc-1"))
    );
    assert_eq!(
        fs::read_to_string(root.path().join(".barracuda/selected-board")).expect("selection file"),
        "esp32c6-devkitc-1\n"
    );
}

#[test]
fn selecting_again_replaces_the_previous_board() {
    let root = tempdir().expect("temporary workspace");
    write_selected_board(root.path(), "local-linux").expect("initial selection");

    write_selected_board(root.path(), "local-macos").expect("replacement selection");

    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        Some(String::from("local-macos"))
    );
}

#[test]
fn malformed_persisted_selection_is_rejected() {
    let root = tempdir().expect("temporary workspace");
    fs::create_dir(root.path().join(".barracuda")).expect("state directory");
    fs::write(
        root.path().join(".barracuda/selected-board"),
        "../../outside\n",
    )
    .expect("invalid selection");

    assert!(matches!(
        read_selected_board(root.path()),
        Err(SelectionError::InvalidName { .. })
    ));
}

#[test]
fn invalid_selection_is_not_written() {
    let root = tempdir().expect("temporary workspace");

    assert!(matches!(
        write_selected_board(root.path(), "local macos"),
        Err(SelectionError::InvalidName { .. })
    ));
    assert!(!root.path().join(".barracuda/selected-board").exists());
}
