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
fn rejects_rust_registration_fields_in_board_yaml() {
    let yaml = r#"
name: product-a
hardware:
  chip: acme123
toolchain:
  target: riscv64acme-unknown-none-elf
native-layout:
  artifact: memory.x
board-hal:
  package: barracuda-board-product-a
  path: platforms/acme/boards/product-a
  type: ProductAHal
"#;

    assert!(matches!(parse(yaml), Err(ConfigError::Yaml(_))));
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
  pins:
    user-control:
      pin: gpio2
    expansion-clock:
      pin: gpio6
    expansion-data:
      pin: gpio7
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

    assert_eq!(io.pin("user-control").map(|pin| pin.pin()), Some("gpio2"));
    assert_eq!(io.pins().count(), 3);
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
fn internal_io_reports_controllers_reserved_from_runtime_pools() {
    let board = parse(
        r#"
name: reserved-controllers
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  i2c-device:
    codec-control:
      peripheral: I2C0
      scl: GPIO1
      sda: GPIO2
      frequency-hz: 400000
  spi-bus:
    pixels:
      peripheral: SPI2
      sck: GPIO3
      mosi: GPIO4
      frequency-hz: 8000000
"#,
    )
    .expect("valid internal resources");

    assert!(board.internal_io().uses_controller("I2C0"));
    assert!(board.internal_io().uses_controller("SPI2"));
    assert!(!board.internal_io().uses_controller("I2C1"));
}

#[test]
fn parses_internal_spi_device_reserved_for_a_builtin_driver() {
    let yaml = r#"
name: display-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  spi-device:
    display-bus:
      peripheral: SPI2
      sck: GPIO6
      mosi: GPIO5
      chip-select: GPIO7
      frequency-hz: 80000000
builtin-peripherals:
  display:
    driver: mipi-dbi-display
    bindings:
      spi: display-bus
      dc: GPIO4
      reset: GPIO8
    parameters:
      controller: gc9a01
      width: 240
      height: 240
"#;

    let board = parse(yaml).expect("valid internal display bus");
    let spi = board
        .internal_io()
        .spi_device("display-bus")
        .expect("display SPI device");

    assert!(board.has_hardware_surface());
    assert_eq!(spi.peripheral(), "SPI2");
    assert_eq!(spi.sck(), "GPIO6");
    assert_eq!(spi.mosi(), Some("GPIO5"));
    assert_eq!(spi.miso(), None);
    assert_eq!(spi.chip_select(), "GPIO7");
    assert_eq!(spi.frequency_hz(), 80_000_000);
}

#[test]
fn parses_driver_data_plane_resources() {
    let board = parse(
        r#"
name: media-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
internal-io:
  spi-bus:
    pixels:
      peripheral: SPI2
      sck: GPIO1
      mosi: GPIO2
      frequency-hz: 2400000
  camera-capture:
    camera:
      peripheral: LCD_CAM
      dma: DMA_CH0
      xclk: GPIO3
      pclk: GPIO4
      vsync: GPIO5
      href: GPIO6
      data: [GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO12, GPIO13, GPIO14]
      xclk-frequency-hz: 20000000
      dma-buffer-bytes: 98304
  i2s-stream:
    audio:
      peripheral: I2S0
      dma: DMA_CH1
      bclk: GPIO15
      ws: GPIO16
      dout: GPIO17
      din: GPIO18
      mclk: GPIO19
      sample-rate-hz: 16000
      channels: 2
      bits-per-sample: 16
      dma-buffer-bytes: 1024
"#,
    )
    .expect("valid Driver data planes");

    let pixels = board.internal_io().spi_bus("pixels").expect("SPI bus");
    assert_eq!(
        (pixels.peripheral(), pixels.frequency_hz()),
        ("SPI2", 2_400_000)
    );
    let camera = board
        .internal_io()
        .camera_capture("camera")
        .expect("camera receiver");
    assert_eq!(
        (camera.data()[7].as_str(), camera.dma_buffer_bytes()),
        ("GPIO14", 98_304)
    );
    let audio = board.internal_io().i2s_stream("audio").expect("I2S stream");
    assert_eq!(
        (
            audio.sample_rate_hz(),
            audio.channels(),
            audio.bits_per_sample(),
            audio.dma_buffer_bytes()
        ),
        (16_000, 2, 16, 1_024)
    );
}

#[test]
fn parses_internal_i2c_device_reserved_for_a_builtin_driver() {
    let board = parse(
        r#"
name: sensor-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  i2c-device:
    environment-bus:
      peripheral: I2C0
      scl: GPIO1
      sda: GPIO2
      frequency-hz: 400000
"#,
    )
    .expect("valid internal I2C bus");
    let i2c = board
        .internal_io()
        .i2c_device("environment-bus")
        .expect("environment I2C bus");

    assert_eq!(i2c.peripheral(), "I2C0");
    assert_eq!(i2c.scl(), "GPIO1");
    assert_eq!(i2c.sda(), "GPIO2");
    assert_eq!(i2c.frequency_hz(), 400_000);
}

#[test]
fn board_without_io_or_builtins_has_no_hardware_surface() {
    let board = parse(VALID).expect("valid Board YAML");

    assert!(!board.has_hardware_surface());
}

#[test]
fn one_exposed_pin_is_not_preassigned_to_a_function() {
    let yaml = r#"
name: muxed-product
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
exposed-io:
  pins:
    raw-line:
      pin: gpio2
"#;

    let board = parse(yaml).expect("multifunction exposed pin");
    assert_eq!(
        board.exposed_io().pin("raw-line").map(|pin| pin.pin()),
        Some("gpio2")
    );
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
  pins:
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
internal-io:
  i2c-device:
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
fn rejects_zero_driver_dma_buffer() {
    let yaml = r#"
name: product-a
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  camera-capture:
    camera:
      peripheral: LCD_CAM
      dma: DMA_CH0
      xclk: GPIO15
      pclk: GPIO13
      vsync: GPIO6
      href: GPIO7
      data: [GPIO11, GPIO9, GPIO8, GPIO10, GPIO12, GPIO18, GPIO17, GPIO16]
      xclk-frequency-hz: 20000000
      dma-buffer-bytes: 0
"#;

    assert_eq!(
        parse(yaml).unwrap_err(),
        ConfigError::ZeroDmaBuffer {
            resource: String::from("camera"),
        }
    );
}

#[test]
fn rejects_spi_without_a_data_pin() {
    let yaml = r#"
name: product-a
hardware:
  chip: esp32c6
native-layout:
  artifact: partitions.csv
internal-io:
  spi-device:
    sensor:
      peripheral: SPI2
      sck: GPIO6
      chip-select: GPIO7
      frequency-hz: 10000000
"#;

    assert_eq!(
        parse(yaml).unwrap_err(),
        ConfigError::MissingSpiDataPin {
            resource: String::from("sensor"),
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
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let configs = workspace.join("boards/configs");
    for entry in std::fs::read_dir(&configs).expect("read Board bundles") {
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
        assert!(!yaml.contains("board-hal:"));
        assert!(!yaml.contains("platform-features:"));
    }
}
