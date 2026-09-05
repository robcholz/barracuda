//! A new Platform and Board require no central Cargo registration.

#![allow(clippy::expect_used)]

use std::fs;

use barracuda_build::prepare_selected_workspace;
use tempfile::tempdir;

#[test]
fn injects_unknown_platform_and_board_hal_as_stable_dependency_aliases() {
    let source = tempdir().expect("source workspace");
    let generated = tempdir().expect("generated workspace");
    let root = source.path();

    write(root, ".barracuda/selected-board", "acme-board\n");
    write(root, "Cargo.toml", "[workspace]\nmembers = []\n");
    write(
        root,
        "platforms/acme/platform.yml",
        "name: acme\npackage: barracuda-platform-acme\ncrate: barracuda_platform_acme\ntype: AcmePlatform\nselection:\n  board-chips: ['acme*']\n  targets:\n    - triple: riscv64acme-unknown-none-elf\nsystem-image:\n  layout:\n    driver: linker-memory\n  flash:\n    driver: command\n    program: flash-acme\n",
    );
    write(
        root,
        "platforms/acme/Cargo.toml",
        "[package]\nname = 'barracuda-platform-acme'\nversion = '0.1.0'\n",
    );
    write(
        root,
        "boards/configs/acme-board/board.yml",
        "name: acme-board\nhardware:\n  chip: acme123\ntoolchain:\n  target: riscv64acme-unknown-none-elf\nnative-layout:\n  artifact: memory.x\nplatform-features: [chip-acme123]\nboard-hal:\n  package: barracuda-board-acme\n  path: platforms/acme/boards/acme-board\n  type: AcmeBoardHal\n",
    );
    write(root, "boards/configs/acme-board/memory.x", "MEMORY {}\n");
    write(
        root,
        "platforms/acme/boards/acme-board/Cargo.toml",
        "[package]\nname = 'barracuda-board-acme'\nversion = '0.1.0'\n",
    );
    write(
        root,
        "platforms/selected/Cargo.toml",
        "[package]\nname = 'selected-platform'\nversion = '0.1.0'\n[dependencies]\n",
    );
    write(
        root,
        "boards/selected/Cargo.toml",
        "[package]\nname = 'selected-board'\nversion = '0.1.0'\n[dependencies]\n",
    );

    let selection = prepare_selected_workspace(root, generated.path()).expect("generated build");

    assert_eq!(selection.board(), "acme-board");
    assert_eq!(selection.platform(), "acme");
    assert_eq!(selection.target(), Some("riscv64acme-unknown-none-elf"));
    let platform_manifest =
        fs::read_to_string(generated.path().join("platforms/selected/Cargo.toml"))
            .expect("selected Platform manifest");
    assert!(platform_manifest.contains("barracuda-selected-platform-implementation"));
    assert!(platform_manifest.contains("barracuda-platform-acme"));
    assert!(platform_manifest.contains("chip-acme123"));
    let board_manifest = fs::read_to_string(generated.path().join("boards/selected/Cargo.toml"))
        .expect("selected Board manifest");
    assert!(board_manifest.contains("barracuda-selected-board-hal-implementation"));
    assert!(board_manifest.contains("barracuda-board-acme"));
}

fn write(root: &std::path::Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent directory")).expect("create directory");
    fs::write(path, contents).expect("write fixture");
}
