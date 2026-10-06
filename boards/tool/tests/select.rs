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
    add_selection_files(root);
}

fn add_selection_files(root: &Path) {
    let platform = root.join("platforms/macos");
    fs::create_dir_all(&platform).expect("Platform directory");
    fs::write(
        platform.join("Cargo.toml"),
        "[package]\nname = \"barracuda-platform-macos\"\nversion = \"0.1.0\"\n",
    )
    .expect("Platform Cargo manifest");
    fs::write(
        platform.join("platform.yml"),
        "name: macos\ninfo:\n  family: macos\n  environment: hosted\npackage: barracuda-platform-macos\ncrate: barracuda_platform_macos\ntype: MacosPlatform\nhal:\n  bindings: [digital-input, digital-output, gpio, spi-device, i2c-device]\nselection:\n  board-chips: [macos]\n  targets:\n    - os: macos\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .barracuda\n    flash-image: board.flash\napplication:\n  support-binaries: [barracuda-macos-network]\n  launcher:\n    program: sudo\n    arguments: [\"{support:barracuda-macos-network}\", \"{application}\"]\n",
    )
    .expect("Platform manifest");
    fs::create_dir_all(root.join("platforms/selected")).expect("selected Platform directory");
    fs::write(
        root.join("platforms/selected/Cargo.toml"),
        "[dependencies]\nbarracuda-platform-selection = { path = \"../../.barracuda/selection/platform\" }\n",
    )
    .expect("selected Platform manifest");
    fs::create_dir_all(root.join("boards/selected")).expect("selected Board directory");
    fs::write(
        root.join("boards/selected/Cargo.toml"),
        "[dependencies]\nbarracuda-board-selection = { path = \"../../.barracuda/selection/board\" }\n",
    )
    .expect("selected Board manifest");
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

    let platform_manifest = fs::read_to_string(root.path().join("platforms/selected/Cargo.toml"))
        .expect("selected Platform manifest");
    let board_manifest = fs::read_to_string(root.path().join("boards/selected/Cargo.toml"))
        .expect("selected Board manifest");

    run(["select", "local-macos"], root.path(), &mut output).expect("select Board");

    assert_eq!(
        read_selected_board(root.path()).expect("read selection"),
        Some(String::from("local-macos"))
    );
    assert_eq!(
        String::from_utf8(output).expect("UTF-8 output"),
        "Selected Board `local-macos`.\nRun `cargo run` to build and start it.\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("platforms/selected/Cargo.toml"))
            .expect("selected Platform manifest after selection"),
        platform_manifest
    );
    assert_eq!(
        fs::read_to_string(root.path().join("boards/selected/Cargo.toml"))
            .expect("selected Board manifest after selection"),
        board_manifest
    );
    let platform = fs::read_to_string(root.path().join(".barracuda/selection/platform/Cargo.toml"))
        .expect("local Platform selection package");
    assert!(platform.contains("default = [\"barracuda-platform-macos\"]"));
    let cargo = fs::read_to_string(root.path().join(".barracuda/cargo.toml"))
        .expect("local Cargo selection");
    assert!(cargo.contains("[build]"));
    assert!(cargo.contains("runner = ["));
    assert!(cargo.contains("\"__run\", \"macos\"]"));
    assert!(root
        .path()
        .join(".barracuda/bin/barracuda-runner")
        .is_file());
}

#[test]
fn select_uses_platform_owned_features_for_the_board_chip() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        &platform_path,
        platform.replace(
            "  targets:\n",
            "  features-by-chip:\n    macos: [native-chip]\n  targets:\n",
        ),
    )
    .expect("Platform manifest with chip features");

    run(["select", "local-macos"], root.path(), &mut Vec::new()).expect("select Board");

    let selected = fs::read_to_string(root.path().join(".barracuda/selection/platform/Cargo.toml"))
        .expect("local Platform selection package");
    assert!(selected.contains(
        "default = [\"barracuda-platform-macos\", \"barracuda-platform-macos/native-chip\"]"
    ));
}

#[test]
fn select_configures_the_esp32p4_c_hard_float_abi() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let board_path = root.path().join("boards/configs/local-macos/board.yml");
    let board = fs::read_to_string(&board_path).expect("Board manifest");
    fs::write(
        &board_path,
        board
            .replace("chip: macos", "chip: macos\n  flash-size: 16mb")
            .replace(
                "native-layout:",
                "toolchain:\n  target: riscv32imafc-unknown-none-elf\nnative-layout:",
            ),
    )
    .expect("ESP32-P4 target");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        &platform_path,
        platform.replace("- os: macos", "- triple: riscv32imafc-unknown-none-elf"),
    )
    .expect("ESP32-P4 platform target");
    let mut output = Vec::new();

    run(["select", "local-macos"], root.path(), &mut output).expect("select Board");

    let cargo = fs::read_to_string(root.path().join(".barracuda/cargo.toml"))
        .expect("local Cargo selection");
    assert!(cargo.contains("[env]"));
    assert!(cargo.contains("CC_riscv32imafc_unknown_none_elf = \"riscv32-esp-elf-gcc\""));
    assert!(cargo.contains("CXX_riscv32imafc_unknown_none_elf = \"riscv32-esp-elf-g++\""));
    assert!(cargo.contains("AR_riscv32imafc_unknown_none_elf = \"riscv32-esp-elf-ar\""));
    assert!(
        cargo.contains("CFLAGS_riscv32imafc_unknown_none_elf = \"-march=rv32imafc -mabi=ilp32f\"")
    );
    assert!(cargo
        .contains("BINDGEN_EXTRA_CLANG_ARGS_riscv32imafc_unknown_none_elf = \"-ffreestanding\""));
    assert!(!cargo.contains("boards/tool/assets"));
    // Rust's own linker links the image, without a C library.
    assert!(!cargo.contains("linker ="));
    assert!(cargo.contains("rustflags = [\"-C\", \"link-arg=-Tlinkall.x\"]"));
    assert!(cargo.contains("\"--launcher-argument=--flash-size\""));
    assert!(cargo.contains("\"--launcher-argument=16mb\""));
    assert!(cargo.contains("\"--launcher-argument=boards/configs/local-macos/file-layout.yml\""));
    assert!(cargo.contains("\"--launcher-argument=ota_0\""));
}

#[test]
fn select_configures_an_xtensa_esp32_target_and_flash_layout() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let board_path = root.path().join("boards/configs/local-macos/board.yml");
    let board = fs::read_to_string(&board_path).expect("Board manifest");
    fs::write(
        &board_path,
        board
            .replace("chip: macos", "chip: macos\n  flash-size: 16mb")
            .replace(
                "native-layout:",
                "toolchain:\n  target: xtensa-esp32s3-none-elf\nnative-layout:",
            ),
    )
    .expect("ESP32-S3 target");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        &platform_path,
        platform.replace("- os: macos", "- triple: xtensa-esp32s3-none-elf"),
    )
    .expect("ESP32-S3 platform target");

    run(["select", "local-macos"], root.path(), &mut Vec::new()).expect("select Board");

    let cargo = fs::read_to_string(root.path().join(".barracuda/cargo.toml"))
        .expect("local Cargo selection");
    assert!(cargo.contains("CC_xtensa_esp32s3_none_elf = \"xtensa-esp32s3-elf-gcc\""));
    assert!(cargo.contains("AR_xtensa_esp32s3_none_elf = \"xtensa-esp32s3-elf-ar\""));
    assert!(cargo.contains("CFLAGS_xtensa_esp32s3_none_elf = \"-mlongcalls\""));
    assert!(cargo.contains("BINDGEN_EXTRA_CLANG_ARGS_xtensa_esp32s3_none_elf ="));
    assert!(cargo.contains("--target=xtensa-esp-elf -I"));
    assert!(cargo.contains("boards/tool/assets/xtensa-include"));
    // The esp-generate flags for espup's Xtensa linker.
    assert!(cargo.contains(
        "rustflags = [\"-C\", \"link-arg=-Tlinkall.x\", \"-C\", \"link-arg=-nostartfiles\"]"
    ));
    assert!(cargo.contains("\"--launcher-argument=--flash-size\""));
    assert!(cargo.contains("\"--launcher-argument=16mb\""));
    assert!(cargo.contains("\"--launcher-argument=boards/configs/local-macos/file-layout.yml\""));
    assert!(cargo.contains("\"--launcher-argument=ota_0\""));
}

#[test]
fn select_links_a_cortex_m_target_with_the_runtime_script() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let board_path = root.path().join("boards/configs/local-macos/board.yml");
    let board = fs::read_to_string(&board_path).expect("Board manifest");
    fs::write(
        &board_path,
        board.replace(
            "native-layout:",
            "toolchain:\n  target: thumbv7em-none-eabihf\nnative-layout:",
        ),
    )
    .expect("STM32 target");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        &platform_path,
        platform.replace("- os: macos", "- triple: thumbv7em-none-eabihf"),
    )
    .expect("STM32 platform target");

    run(["select", "local-macos"], root.path(), &mut Vec::new()).expect("select Board");

    let cargo = fs::read_to_string(root.path().join(".barracuda/cargo.toml"))
        .expect("local Cargo selection");
    // embassy-stm32's example flags: cortex-m-rt's script over the Platform's memory.x.
    assert!(cargo.contains(
        "[target.thumbv7em-none-eabihf]\nrustflags = [\"-C\", \"link-arg=--nmagic\", \"-C\", \"link-arg=-Tlink.x\"]"
    ));
    assert!(!cargo.contains("linker ="));
}

#[test]
fn select_uses_the_platform_hal_without_a_chip_adapter() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "local-macos", "local-macos");
    let board_path = root.path().join("boards/configs/local-macos/board.yml");
    let board = fs::read_to_string(&board_path).expect("Board YAML");
    fs::write(
        &board_path,
        format!(
            "{}exposed-io:\n  pins:\n    button:\n      pin: GPIO0\n",
            board.replace("chip: macos", "chip: esp32")
        ),
    )
    .expect("Board hardware surface");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        platform_path,
        platform.replace("board-chips: [macos]", "board-chips: [macos, esp32]"),
    )
    .expect("Platform chip selection");
    fs::create_dir_all(root.path().join("peripherals/impl"))
        .expect("peripheral implementation catalog");

    run(["select", "local-macos"], root.path(), &mut Vec::new()).expect("select Board");

    let selected = fs::read_to_string(root.path().join(".barracuda/selection/board/Cargo.toml"))
        .expect("local Board selection package");
    assert!(!selected.contains("barracuda-peripheral-gpio"));
    assert!(!selected.contains("barracuda-platform-"));
}

#[test]
fn select_adds_only_an_unknown_implementation_of_a_registered_peripheral_api() {
    let root = tempdir().expect("temporary workspace");
    add_board(root.path(), "sensor-board", "sensor-board");
    fs::write(
        root.path().join("boards/configs/sensor-board/board.yml"),
        r#"
name: sensor-board
hardware:
  chip: esp32
native-layout:
  artifact: file-layout.yml
peripherals:
  io:
    i2c-device:
      sensor:
        peripheral: I2C0
        scl: GPIO1
        sda: GPIO2
        frequency-hz: 400000
    spi-device:
      auxiliary:
        peripheral: SPI2
        sck: GPIO3
        mosi: GPIO4
        chip-select: GPIO5
        frequency-hz: 10000000
  devices:
    environment:
      implementation: future-sensor
      bindings:
        i2c: sensor
        spi: auxiliary
"#,
    )
    .expect("Board YAML");
    let platform_path = root.path().join("platforms/macos/platform.yml");
    let platform = fs::read_to_string(&platform_path).expect("Platform manifest");
    fs::write(
        platform_path,
        platform.replace("board-chips: [macos]", "board-chips: [macos, esp32]"),
    )
    .expect("Platform chip selection");
    let driver = root
        .path()
        .join("peripherals/impl/power-monitor/future-sensor");
    fs::create_dir_all(&driver).expect("peripheral implementation directory");
    fs::write(
        driver.join("peripheral.yml"),
        r#"
id: future-sensor
api-version: 1
peripheral: power-monitor
implementation:
  package: future-sensor
  crate: future_sensor
  factory: "::{{crate}}::Implementation<{{binding.i2c.type}}, {{binding.spi.type}}>"
  bindings-expression: "::{{crate}}::Bindings::new({{binding.i2c.value}}, {{binding.spi.value}})"
  config-expression: "()"
bindings:
  i2c:
    kind: i2c-device
  spi:
    kind: spi-device
parameters: {}
"#,
    )
    .expect("peripheral implementation manifest");
    fs::write(
        driver.join("Cargo.toml"),
        "[package]\nname = \"future-sensor\"\nversion = \"0.1.0\"\n",
    )
    .expect("peripheral Cargo manifest");

    run(["select", "sensor-board"], root.path(), &mut Vec::new()).expect("select Board");

    let selected = fs::read_to_string(root.path().join(".barracuda/selection/board/Cargo.toml"))
        .expect("local Board selection package");
    assert!(selected.contains("default = [\"future-sensor\"]"));
    assert!(!selected.contains("barracuda-peripheral-i2c"));
    assert!(!selected.contains("barracuda-peripheral-spi"));
    assert!(!selected.contains("barracuda-peripheral-gpio"));
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

    assert!(config.contains("include = [{ path = \"../.barracuda/cargo.toml\", optional = true }]"));
    for alias in ["board", "cli", "platform", "plugin", "image"] {
        let line = config
            .lines()
            .find(|line| line.starts_with(&format!("{alias} = ")))
            .expect("host Cargo alias");
        let isolated_target = if alias == "board" {
            "target/board-bootstrap"
        } else {
            "target/host-tools"
        };
        assert!(
            line.contains("unstable.build-std=[\\\"std\\\"]")
                && line.contains(isolated_target)
                && line.contains("host-tuple"),
            "host alias `{alias}` must build a complete host standard library in an isolated target directory"
        );
    }
    assert!(!config
        .lines()
        .any(|line| line.trim_start().starts_with("build =")));
}

#[test]
fn normal_cargo_build_targets_the_selected_application_directly() {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .expect("workspace manifest");
    let default_members = manifest
        .split_once("default-members = [")
        .and_then(|(_before, after)| after.split_once(']'))
        .map(|(members, _after)| members)
        .expect("default members");

    assert!(default_members.contains("apps/barracuda-system"));
    assert!(!default_members.contains("tools/barracuda-build"));
    assert!(!manifest.contains("\"tools/barracuda-build\","));
    assert!(!manifest.contains("boards/chips"));
    assert!(!manifest.contains("\"boards/configs/*/hal\","));
    assert!(!manifest.contains("\"boards/stm32f429zi-nucleo\","));
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
