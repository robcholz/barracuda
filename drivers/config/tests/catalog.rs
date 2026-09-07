//! Driver catalog behavior.

#![allow(clippy::expect_used)]

use std::fs;

use barracuda_board_config::{PeripheralParameter, parse};
use barracuda_driver_config::{ResolveError, load_catalog, render_board_hal, resolve_board};
use tempfile::tempdir;

const INDICATOR_MANIFEST: &str = r#"
id: indicator-led
api-version: 1
capability: indicator
implementation:
  package: barracuda-indicator-led
  crate: barracuda_indicator_led
  driver-type: IndicatorLedDriver
bindings:
  pin:
    kind: digital-output
parameters:
  active-level:
    type: enum
    values: [low, high]
    required: true
"#;

const DISPLAY_MANIFEST: &str = r#"
id: test-display
api-version: 1
capability: display
implementation:
  package: test-display
  crate: test_display
  driver-type: TestDisplayDriver
bindings:
  spi:
    kind: spi-device
  dc:
    kind: digital-output
parameters: {}
"#;

fn board(driver: &str, bindings: &str, parameters: &str) -> String {
    let bindings = if bindings.is_empty() {
        String::from("    bindings: {}\n")
    } else {
        format!("    bindings:\n{bindings}")
    };
    format!(
        "name: test-board\nhardware:\n  chip: test-chip\nnative-layout:\n  artifact: memory.x\nbuiltin-peripherals:\n  status-led:\n    driver: {driver}\n{bindings}    parameters:\n{parameters}"
    )
}

fn add_driver(root: &std::path::Path, directory: &str, manifest: &str) {
    let path = root.join("drivers").join(directory);
    fs::create_dir_all(&path).expect("driver directory");
    fs::write(path.join("driver.yml"), manifest).expect("driver manifest");
}

#[test]
fn resolves_a_board_against_driver_owned_schemas() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(&board(
        "indicator-led",
        "      pin: PB0\n",
        "      active-level: high\n",
    ))
    .expect("Board YAML");

    let resolved = resolve_board(&board, &catalog).expect("resolved composition");
    let peripheral = resolved.peripheral("status-led").expect("status LED");

    assert_eq!(peripheral.driver().id(), "indicator-led");
    assert_eq!(peripheral.driver().capability(), "indicator");
    assert_eq!(peripheral.binding("pin"), Some("PB0"));
    assert_eq!(
        peripheral.parameter("active-level"),
        Some(&PeripheralParameter::String(String::from("high")))
    );
}

#[test]
fn rejects_an_unknown_driver_id() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(&board(
        "typo-driver",
        "      pin: PB0\n",
        "      active-level: high\n",
    ))
    .expect("Board YAML");

    assert!(matches!(
        resolve_board(&board, &catalog),
        Err(ResolveError::UnknownDriver { .. })
    ));
}

#[test]
fn rejects_missing_and_unknown_bindings() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");

    let missing =
        parse(&board("indicator-led", "", "      active-level: high\n")).expect("Board YAML");
    assert!(matches!(
        resolve_board(&missing, &catalog),
        Err(ResolveError::MissingBinding { .. })
    ));

    let unknown = parse(&board(
        "indicator-led",
        "      pin: PB0\n      typo: PB1\n",
        "      active-level: high\n",
    ))
    .expect("Board YAML");
    assert!(matches!(
        resolve_board(&unknown, &catalog),
        Err(ResolveError::UnknownBinding { .. })
    ));
}

#[test]
fn rejects_unknown_and_invalid_parameters() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");

    let unknown = parse(&board(
        "indicator-led",
        "      pin: PB0\n",
        "      active-level: high\n      typo: true\n",
    ))
    .expect("Board YAML");
    assert!(matches!(
        resolve_board(&unknown, &catalog),
        Err(ResolveError::UnknownParameter { .. })
    ));

    let invalid = parse(&board(
        "indicator-led",
        "      pin: PB0\n",
        "      active-level: sideways\n",
    ))
    .expect("Board YAML");
    assert!(matches!(
        resolve_board(&invalid, &catalog),
        Err(ResolveError::InvalidParameter { .. })
    ));
}

#[test]
fn resolves_spi_devices_through_board_internal_io() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-display", DISPLAY_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let valid = parse(
        r#"
name: display-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
internal-io:
  spi-device:
    display:
      peripheral: SPI2
      sck: GPIO6
      mosi: GPIO5
      chip-select: GPIO7
      frequency-hz: 40000000
builtin-peripherals:
  display:
    driver: test-display
    bindings:
      spi: display
      dc: GPIO4
"#,
    )
    .expect("Board YAML");
    resolve_board(&valid, &catalog).expect("resolved SPI device");

    let invalid = parse(
        r#"
name: display-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
builtin-peripherals:
  display:
    driver: test-display
    bindings:
      spi: missing
      dc: GPIO4
"#,
    )
    .expect("Board YAML");
    assert!(matches!(
        resolve_board(&invalid, &catalog),
        Err(ResolveError::UnknownInternalResource { .. })
    ));
}

#[test]
fn rejects_duplicate_ids_and_directory_id_mismatches() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "wrong-directory", INDICATOR_MANIFEST);

    assert!(load_catalog(root.path()).is_err());
}

#[test]
fn renders_a_static_hal_from_only_board_and_driver_yaml() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(
        r#"
name: nucleo-copy
hardware:
  chip: stm32f429zi
native-layout:
  artifact: memory.x
builtin-peripherals:
  status-led:
    driver: indicator-led
    bindings:
      pin: PB0
    parameters:
      active-level: high
exposed-io:
  gpio:
    user-button:
      pin: PC13
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved composition");

    let rust = render_board_hal(&board, &resolved).expect("generated HAL");

    assert!(rust.contains("pub struct GeneratedBoardBindings"));
    assert!(rust.contains("peripherals::PB0"));
    assert!(rust.contains("peripherals::PC13"));
    assert!(rust.contains("IndicatorLedDriver"));
    assert!(rust.contains("PeripheralDriver>::initialize"));
    assert!(rust.contains("impl BuiltinIndicator for GeneratedBuiltins"));
    assert!(rust.contains("\"user-button\""));
    assert!(!rust.contains("nucleo-copy"));
}

#[test]
fn renders_esp32s3_mipi_display_from_board_resources() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "mipi-dbi-display",
        r#"
id: mipi-dbi-display
api-version: 1
capability: display
implementation:
  package: barracuda-mipi-dbi-display
  crate: barracuda_mipi_dbi_display
  driver-type: MipiDbiDisplayDriver
bindings:
  spi:
    kind: spi-device
  dc:
    kind: digital-output
  reset:
    kind: digital-output
  backlight:
    kind: digital-output
parameters:
  controller:
    type: enum
    values: [gc9a01]
    required: true
  width:
    type: integer
    required: true
  height:
    type: integer
    required: true
  offset-x:
    type: integer
    default: 0
  offset-y:
    type: integer
    default: 0
  color-order:
    type: enum
    values: [rgb, bgr]
    default: bgr
  invert-colors:
    type: boolean
    default: false
  orientation:
    type: enum
    values: [deg0, deg90, deg180, deg270]
    default: deg0
  backlight-active-high:
    type: boolean
    default: true
"#,
    );
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(
        r#"
name: dial-copy
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  spi-device:
    display-spi:
      peripheral: SPI2
      sck: GPIO6
      mosi: GPIO5
      chip-select: GPIO7
      frequency-hz: 80000000
builtin-peripherals:
  display:
    driver: mipi-dbi-display
    bindings:
      spi: display-spi
      dc: GPIO4
      reset: GPIO8
      backlight: GPIO9
    parameters:
      controller: gc9a01
      width: 240
      height: 240
      invert-colors: true
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved display");

    let rust = render_board_hal(&board, &resolved).expect("generated HAL");

    assert!(rust.contains("peripherals::SPI2<'static>"));
    assert!(rust.contains("peripherals::GPIO7<'static>"));
    assert!(rust.contains("MipiDbiDisplayDriver"));
    assert!(rust.contains("GC9A01"));
    assert!(rust.contains("Size::new(240, 240)"));
    assert!(rust.contains("with_inverted_colors(true)"));
    assert!(rust.contains("impl BuiltinDisplay for GeneratedBuiltins"));
    assert!(!rust.contains("dial-copy"));
}

#[test]
fn every_repository_builtin_composition_resolves_and_generates() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let catalog = load_catalog(root).expect("repository Driver catalog");
    let entries = fs::read_dir(root.join("boards/configs")).expect("Board config directory");

    for entry in entries {
        let entry = entry.expect("Board directory");
        if !entry.file_type().expect("Board entry type").is_dir() {
            continue;
        }
        let path = entry.path().join("board.yml");
        let yaml = fs::read_to_string(&path).expect("Board YAML");
        let board = parse(&yaml).expect("valid Board YAML");
        if !board.has_hardware_surface() {
            continue;
        }
        let resolved = resolve_board(&board, &catalog).expect("repository Board resolves");
        render_board_hal(&board, &resolved).expect("repository Board HAL generates");
    }
}
