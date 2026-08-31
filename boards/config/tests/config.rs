//! Board YAML parsing, validation, and code generation tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use barracuda_board_config::{parse, render_rust, ConfigError, PeripheralParameter};
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
fn parses_explicit_io_and_builtin_peripheral_declarations() {
    let yaml = r#"
name: product-a
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
exposed-io:
  gpio:
    user-control:
      pin: gpio2
  analog-input:
    sensor-voltage:
      peripheral: adc1
      pin: gpio3
      channel: 2
  analog-output:
    reference-voltage:
      peripheral: dac1
      pin: gpio4
      channel: 1
  pwm:
    actuator:
      peripheral: ledc0
      pin: gpio5
      channel: 0
  i2c:
    expansion:
      peripheral: i2c0
      scl: gpio6
      sda: gpio7
      frequency-hz: 400000
  spi:
    storage:
      peripheral: spi2
      sck: gpio8
      mosi: gpio9
      miso: gpio10
      frequency-hz: 20000000
builtin-peripherals:
  indicator:
    driver: gpio-indicator
    bindings:
      pin: gpio2
    parameters:
      active-low: true
      brightness: 128
      modes: [steady, pulse]
"#;

    let board = parse(yaml).expect("valid hardware surface");
    assert!(board.has_hardware_surface());
    let io = board.exposed_io();

    assert_eq!(
        io.gpio("user-control").map(|gpio| gpio.pin()),
        Some("gpio2")
    );
    assert_eq!(
        io.analog_input("sensor-voltage").map(|input| (
            input.peripheral(),
            input.pin(),
            input.channel()
        )),
        Some(("adc1", "gpio3", 2))
    );
    assert_eq!(
        io.analog_output("reference-voltage").map(|output| (
            output.peripheral(),
            output.pin(),
            output.channel()
        )),
        Some(("dac1", "gpio4", 1))
    );
    assert_eq!(
        io.pwm("actuator")
            .map(|pwm| (pwm.peripheral(), pwm.pin(), pwm.channel())),
        Some(("ledc0", "gpio5", 0))
    );
    assert_eq!(
        io.i2c("expansion")
            .map(|i2c| (i2c.peripheral(), i2c.scl(), i2c.sda(), i2c.frequency_hz())),
        Some(("i2c0", "gpio6", "gpio7", 400_000))
    );
    assert_eq!(
        io.spi("storage").map(|spi| {
            (
                spi.peripheral(),
                spi.sck(),
                spi.mosi(),
                spi.miso(),
                spi.frequency_hz(),
            )
        }),
        Some(("spi2", "gpio8", Some("gpio9"), Some("gpio10"), 20_000_000))
    );
    assert_eq!(
        board
            .builtin_peripheral("indicator")
            .map(|builtin| (builtin.driver(), builtin.binding("pin"))),
        Some(("gpio-indicator", Some("gpio2")))
    );
    let indicator = board
        .builtin_peripheral("indicator")
        .expect("indicator declaration");
    assert_eq!(
        indicator.parameter("active-low"),
        Some(&PeripheralParameter::Boolean(true))
    );
    assert_eq!(
        indicator.parameter("brightness"),
        Some(&PeripheralParameter::Integer(128))
    );
    assert_eq!(
        indicator.parameter("modes"),
        Some(&PeripheralParameter::Sequence(vec![
            PeripheralParameter::String(String::from("steady")),
            PeripheralParameter::String(String::from("pulse")),
        ]))
    );
}

#[test]
fn board_without_io_or_builtins_has_no_hardware_surface() {
    let board = parse(VALID).expect("valid Board YAML");

    assert!(!board.has_hardware_surface());
}

#[test]
fn accepts_repeated_physical_identifiers_across_declarations() {
    let yaml = r#"
name: muxed-product
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
exposed-io:
  gpio:
    raw-line:
      pin: gpio2
  i2c:
    expansion:
      peripheral: i2c0
      scl: gpio2
      sda: gpio3
      frequency-hz: 100000
builtin-peripherals:
  indicator:
    driver: gpio-indicator
    bindings:
      pin: gpio2
"#;

    parse(yaml).expect("the common schema assigns no overlap policy");
}

#[test]
fn rejects_empty_declared_hardware_identifiers() {
    let yaml = r#"
name: product-a
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
exposed-io:
  gpio:
    user-control:
      pin: ''
"#;

    assert_eq!(
        parse(yaml).unwrap_err(),
        ConfigError::EmptyHardwareIdentifier {
            resource: String::from("user-control"),
            field: "pin",
        }
    );
}

#[test]
fn rejects_zero_protocol_frequency() {
    let yaml = r#"
name: product-a
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
exposed-io:
  i2c:
    expansion:
      peripheral: i2c0
      scl: gpio6
      sda: gpio7
      frequency-hz: 0
"#;

    assert_eq!(
        parse(yaml).unwrap_err(),
        ConfigError::ZeroProtocolFrequency {
            resource: String::from("expansion"),
        }
    );
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
