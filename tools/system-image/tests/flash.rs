//! Selected-Board resource partition flashing.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_system_image::{flash_selected, IMAGE_OUTPUT};
use tempfile::tempdir;

const CAPACITY: usize = 32 * 1024;
const RESOURCES_OFFSET: usize = 4 * 1024;
const RESOURCES_SIZE: usize = 8 * 1024;

fn select_host_board(root: &Path, chip: &str) {
    let board_name = format!("local-{chip}");
    let bundle = root.join("boards/configs").join(&board_name);
    fs::create_dir_all(&bundle).expect("Board bundle");
    fs::create_dir_all(root.join(".barracuda")).expect("selection directory");
    fs::write(
        root.join(".barracuda/selected-board"),
        format!("{board_name}\n"),
    )
    .expect("selected Board");
    fs::write(
        bundle.join("board.yml"),
        format!(
            "name: {board_name}\nhardware:\n  chip: {chip}\nnative-layout:\n  artifact: file-layout.yml\n"
        ),
    )
    .expect("Board definition");
    fs::write(
        bundle.join("file-layout.yml"),
        format!(
            "capacity: {CAPACITY}\nregions:\n  - name: resources\n    offset: {RESOURCES_OFFSET}\n    size: {RESOURCES_SIZE}\n    access: read-only\n    filesystem: fatfs\n"
        ),
    )
    .expect("native layout");

    let platform = root.join("platforms").join(chip);
    fs::create_dir_all(&platform).expect("Platform directory");
    fs::write(
        platform.join("platform.yml"),
        format!(
            "name: {chip}\npackage: test-platform-{chip}\ncrate: test_platform_{chip}\ntype: TestPlatform\nselection:\n  board-chips: [{chip}]\n  targets:\n    - os: test\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state-{chip}\n    flash-image: physical.flash\n"
        ),
    )
    .expect("Platform definition");
}

fn write_system_image(root: &Path, fill: u8, size: usize) {
    let output = root.join(IMAGE_OUTPUT);
    fs::create_dir_all(output.parent().expect("output parent")).expect("target directory");
    fs::write(output, vec![fill; size]).expect("System image");
}

#[test]
fn flashes_only_the_selected_partition_of_the_board_platform_file() {
    let root = tempdir().expect("temporary workspace");
    select_host_board(root.path(), "hosta");
    write_system_image(root.path(), 0x5a, RESOURCES_SIZE);
    let flash_path = root.path().join(".state-hosta/physical.flash");
    fs::create_dir_all(flash_path.parent().expect("flash parent")).expect("state directory");
    fs::write(&flash_path, vec![0xa5; CAPACITY]).expect("physical flash");

    let flashed = flash_selected(root.path()).expect("flash selected System partition");

    assert_eq!(flashed.board(), "local-hosta");
    assert_eq!(flashed.image(), root.path().join(IMAGE_OUTPUT));
    assert_eq!(flashed.destination(), flash_path.display().to_string());
    assert_eq!(flashed.offset(), RESOURCES_OFFSET as u64);
    assert_eq!(flashed.size(), RESOURCES_SIZE);
    let physical = fs::read(flashed.destination()).expect("flashed bytes");
    assert_eq!(&physical[..RESOURCES_OFFSET], &vec![0xa5; RESOURCES_OFFSET]);
    assert_eq!(
        &physical[RESOURCES_OFFSET..RESOURCES_OFFSET + RESOURCES_SIZE],
        &vec![0x5a; RESOURCES_SIZE]
    );
    assert_eq!(
        &physical[RESOURCES_OFFSET + RESOURCES_SIZE..],
        &vec![0xa5; CAPACITY - RESOURCES_OFFSET - RESOURCES_SIZE]
    );
}

#[test]
fn initializes_a_missing_host_flash_as_erased_before_writing_the_partition() {
    let root = tempdir().expect("temporary workspace");
    select_host_board(root.path(), "hostb");
    write_system_image(root.path(), 0x36, RESOURCES_SIZE);

    let flashed = flash_selected(root.path()).expect("flash selected System partition");

    let physical = fs::read(flashed.destination()).expect("flashed bytes");
    assert_eq!(physical.len(), CAPACITY);
    assert!(physical[..RESOURCES_OFFSET]
        .iter()
        .all(|byte| *byte == 0xff));
    assert!(
        physical[RESOURCES_OFFSET..RESOURCES_OFFSET + RESOURCES_SIZE]
            .iter()
            .all(|byte| *byte == 0x36)
    );
    assert!(physical[RESOURCES_OFFSET + RESOURCES_SIZE..]
        .iter()
        .all(|byte| *byte == 0xff));
}

#[test]
fn rejects_an_image_that_does_not_exactly_fill_the_selected_partition() {
    let root = tempdir().expect("temporary workspace");
    select_host_board(root.path(), "hosta");
    write_system_image(root.path(), 0x11, RESOURCES_SIZE - 1);

    let error = flash_selected(root.path()).expect_err("wrong image size must fail");

    assert!(error.contains("8191 bytes"));
    assert!(error.contains("8192-byte"));
    assert!(!root.path().join(".state-hosta/physical.flash").exists());
}

#[test]
fn rejects_a_host_flash_whose_capacity_disagrees_with_the_board_layout() {
    let root = tempdir().expect("temporary workspace");
    select_host_board(root.path(), "hostb");
    write_system_image(root.path(), 0x22, RESOURCES_SIZE);
    let flash_path = root.path().join(".state-hostb/physical.flash");
    fs::create_dir_all(flash_path.parent().expect("flash parent")).expect("state directory");
    fs::write(&flash_path, vec![0x77; CAPACITY - 1]).expect("physical flash");

    let error = flash_selected(root.path()).expect_err("wrong flash capacity must fail");

    assert!(error.contains("32767 bytes"));
    assert!(error.contains("32768"));
    assert_eq!(
        fs::read(&flash_path).expect("unchanged physical flash"),
        vec![0x77; CAPACITY - 1]
    );
}
