//! Driver catalog behavior.

#![allow(clippy::expect_used)]

use std::fs;

use barracuda_board_config::{PeripheralParameter, parse};
use barracuda_driver_config::{
    ResolveError, load_catalog, render_board_hal, render_board_hal_for_platform, resolve_board,
};
use barracuda_platform_config::{discover_platforms, resolve_board_platform};
use tempfile::tempdir;

const INDICATOR_MANIFEST: &str = r#"
id: indicator-led
api-version: 1
capability: indicator
implementation:
  package: barracuda-indicator-led
  crate: barracuda_indicator_led
  factory: "::{{crate}}::IndicatorLedDriver<{{binding.pin.type}}>"
  bindings-expression: "{{binding.pin.value}}"
  config-expression: "::barracuda_driver::indicator::IndicatorConfig::new({{parameter.active-level}})"
bindings:
  pin:
    kind: digital-output
    initial-from:
      parameter: active-level
      values: { low: high, high: low }
parameters:
  active-level:
    type: enum
    values: [low, high]
    rust-values:
      low: "::barracuda_driver::indicator::ActiveLevel::Low"
      high: "::barracuda_driver::indicator::ActiveLevel::High"
    required: true
"#;

const DISPLAY_MANIFEST: &str = r#"
id: test-display
api-version: 1
capability: display
implementation:
  package: test-display
  crate: test_display
  factory: "::{{crate}}::TestDisplayDriver<{{binding.spi.type}}, {{binding.dc.type}}>"
  bindings-expression: "::{{crate}}::Bindings::new({{binding.spi.value}}, {{binding.dc.value}})"
  config-expression: "()"
bindings:
  spi:
    kind: spi-device
  dc:
    kind: digital-output
    initial: low
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

fn assert_valid_rust(source: &str) {
    syn::parse_file(source).expect("generated source is valid Rust");
}

#[test]
fn platform_runtime_controllers_are_composed_without_board_protocol_configuration() {
    let root = tempdir().expect("temporary workspace");
    fs::create_dir_all(root.path().join("drivers")).expect("driver catalog");
    let platform_directory = root.path().join("platforms/acme");
    fs::create_dir_all(&platform_directory).expect("Platform directory");
    fs::write(
        platform_directory.join("platform.yml"),
        r#"
name: acme
package: barracuda-platform-acme
crate: barracuda_platform_acme
type: AcmePlatform
hal:
  bindings: [gpio, i2c-device, spi-bus]
  runtime-i2c-controllers: [I2C0, I2C1]
  runtime-spi-controllers: [SPI2]
  runtime-uart-controllers: [UART1]
  runtime-adc-controllers:
    - controller: ADC0
      channels:
        - channel: CHANNEL0
          pin: GPIO1
  runtime-pwm-controllers:
    - controller: PWM0
      timers: [TIMER0]
      channels: [CHANNEL0]
  runtime-i2s-controllers:
    - controller: I2S0
      dma-channels: [DMA0]
      dma-buffer-bytes: 1024
    - controller: I2S1
      dma-channels: [DMA1]
      dma-buffer-bytes: 2048
selection:
  board-chips: [acme]
  targets:
    - triple: acme-none-elf
system-image:
  layout:
    driver: file-regions
  flash:
    driver: file
    state-directory: .state
    flash-image: flash.bin
"#,
    )
    .expect("Platform manifest");
    let board = parse(
        r#"
name: expansion-board
hardware:
  chip: acme
native-layout:
  artifact: memory.yml
internal-io:
  i2c-device:
    builtin-control:
      peripheral: I2C0
      scl: GPIO8
      sda: GPIO9
      frequency-hz: 400000
  i2s-stream:
    builtin-audio:
      peripheral: I2S0
      dma: DMA0
      bclk: GPIO10
      ws: GPIO11
      dout: GPIO12
      din: GPIO13
      sample-rate-hz: 48000
      channels: 2
      bits-per-sample: 16
      dma-buffer-bytes: 1024
exposed-io:
  pins:
    clock: { pin: GPIO1 }
    data: { pin: GPIO2 }
"#,
    )
    .expect("Board YAML");
    let catalog = load_catalog(root.path()).expect("empty Driver catalog");
    let resolved = resolve_board(&board, &catalog).expect("empty built-in composition");
    let platform = discover_platforms(root.path())
        .expect("Platform catalog")
        .pop()
        .expect("Platform");

    let rust =
        render_board_hal_for_platform(&board, &resolved, &platform).expect("generated runtime I/O");
    assert_valid_rust(&rust);
    assert!(rust.contains("hal::RuntimeIo<2, 1, 1, 1, 1, 1, 1>"));
    assert!(rust.contains("controller_binding_type!(I2C1)"));
    assert!(!rust.contains("controller_binding_type!(I2C0)"));
    assert!(rust.contains("controller_binding_type!(SPI2)"));
    assert!(rust.contains("hal::runtime_pin(bindings.pin_clock)"));
    assert!(rust.contains("hal::runtime_i2c_controller"));
    assert!(rust.contains("hal::runtime_spi_controller"));
    assert!(rust.contains("controller_binding_type!(UART1)"));
    assert!(rust.contains("hal::runtime_uart_controller"));
    assert!(rust.contains("controller_binding_type!(ADC0)"));
    assert!(rust.contains("hal::runtime_adc_channel!(CHANNEL0, GPIO1)"));
    assert!(rust.contains("controller_binding_type!(PWM0)"));
    assert!(rust.contains("hal::runtime_pwm_timer!(TIMER0)"));
    assert!(rust.contains("hal::runtime_pwm_channel!(CHANNEL0)"));
    assert!(rust.contains("controller_binding_type!(I2S1)"));
    assert!(rust.contains("controller_binding_type!(DMA1)"));
    assert!(!rust.contains("controller_binding_type!(I2S0)"));
    assert!(!rust.contains("controller_binding_type!(DMA0)"));
    assert!(rust.contains("hal::runtime_i2s_dma_buffers!(2048)"));
}

#[test]
fn repository_esp32s3_devkit_exposes_runtime_gpio_i2c_and_spi() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let board = parse(
        &fs::read_to_string(root.join("boards/configs/esp32s3-devkitc-1/board.yml"))
            .expect("repository Board"),
    )
    .expect("valid repository Board");
    let catalog = load_catalog(&root).expect("repository Driver catalog");
    let resolved = resolve_board(&board, &catalog).expect("resolved repository Board");
    let platform = resolve_board_platform(
        &root,
        board.hardware().chip(),
        board.toolchain().map(|toolchain| toolchain.target()),
    )
    .expect("ESP32-S3 Platform");

    let rust = render_board_hal_for_platform(&board, &resolved, &platform)
        .expect("generated ESP32-S3 runtime owner");
    assert_valid_rust(&rust);
    assert!(rust.contains("hal::RuntimeIo<4, 2, 2, 1, 0, 0, 0>"));
    assert!(rust.contains("controller_binding_type!(I2C0)"));
    assert!(rust.contains("controller_binding_type!(I2C1)"));
    assert!(rust.contains("controller_binding_type!(SPI2)"));
    assert!(rust.contains("controller_binding_type!(SPI3)"));
    assert!(rust.contains("controller_binding_type!(UART1)"));
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
      miso: GPIO3
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
    let resolved = resolve_board(&valid, &catalog).expect("resolved SPI device");
    let rust = render_board_hal(&valid, &resolved).expect("generated SPI device");
    assert!(rust.contains("pin_binding_type!(GPIO3)"));
    assert!(rust.contains("hal::spi_device_full_duplex"));

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
fn renders_media_data_planes_without_driver_specific_generator_code() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "test-media",
        r#"
id: test-media
api-version: 1
capability: test-media
implementation:
  package: test-media
  crate: test_media
  factory: "::{{crate}}::Driver<{{binding.pixels.type}}, {{binding.camera.type}}, {{binding.audio.type}}>"
  bindings-expression: "::{{crate}}::Bindings::new({{binding.pixels.value}}, {{binding.camera.value}}, {{binding.audio.value}})"
  config-expression: "()"
bindings:
  pixels:
    kind: spi-bus
  camera:
    kind: camera-capture
  audio:
    kind: i2s-stream
parameters: {}
"#,
    );
    let catalog = load_catalog(root.path()).expect("driver catalog");
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
      sample-rate-hz: 16000
      channels: 2
      bits-per-sample: 16
      dma-buffer-bytes: 1024
builtin-peripherals:
  media:
    driver: test-media
    bindings: { pixels: pixels, camera: camera, audio: audio }
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved media Driver");
    let rust = render_board_hal(&board, &resolved).expect("generated media HAL");

    assert_valid_rust(&rust);
    assert!(rust.contains("hal::spi_bus("));
    assert!(rust.contains("hal::camera_dma_buffer!(98304)"));
    assert!(rust.contains("hal::camera_capture("));
    assert!(rust.contains("hal::i2s_dma_buffers!(1024)"));
    assert!(rust.contains("hal::i2s_stream("));
    assert!(rust.contains("test_media::Driver"));
}

#[test]
fn rejects_duplicate_ids_and_directory_id_mismatches() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "wrong-directory", INDICATOR_MANIFEST);

    assert!(load_catalog(root.path()).is_err());
}

#[test]
fn rejects_unknown_driver_template_placeholders_during_catalog_load() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "invalid-driver",
        r#"
id: invalid-driver
api-version: 1
capability: test
implementation:
  package: invalid-driver
  crate: invalid_driver
  factory: "::{{crate}}::Driver<{{binding.typo.type}}>"
  bindings-expression: "()"
  config-expression: "()"
bindings: {}
parameters: {}
"#,
    );

    let error = load_catalog(root.path()).expect_err("invalid template placeholder");

    assert!(error.to_string().contains("binding.typo.type"));
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
  pins:
    user-button:
      pin: PC13
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved composition");

    let rust = render_board_hal(&board, &resolved).expect("generated HAL");
    assert_valid_rust(&rust);

    assert!(rust.contains("pub struct GeneratedBoardBindings"));
    assert!(rust.contains("binding_type!(PB0)"));
    assert!(rust.contains("binding_type!(PC13)"));
    assert!(rust.contains("IndicatorLedDriver"));
    assert!(rust.contains("PeripheralDriver>::initialize"));
    assert!(rust.contains("indicator::BuiltinIndicator for GeneratedBuiltins"));
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
  factory: "::{{crate}}::MipiDbiDisplayDriver<{{binding.spi.type}}, {{binding.dc.type}}, {{parameter.controller}}, {{binding.reset.type}}, {{binding.backlight.type}}, {{hal.delay.type}}, 512>"
  bindings-expression: "::{{crate}}::MipiDbiDisplayBindings::new({{binding.spi.value}}, {{binding.dc.value}}, {{binding.reset.value}}, Some({{binding.backlight.value}}), {{hal.delay.value}}, ::{{crate}}::take_transfer_buffer())"
  config-expression: "::{{crate}}::MipiDbiDriverConfig::new({{parameter.controller}}, ::{{crate}}::MipiDbiDisplayConfig::new(::{{crate}}::Size::new({{parameter.width}}, {{parameter.height}}), ::barracuda_driver::display::PixelFormat::Rgb565, {{parameter.orientation}}).with_offset({{parameter.offset-x}}, {{parameter.offset-y}}).with_color_order({{parameter.color-order}}).with_inverted_colors({{parameter.invert-colors}}).with_digital_backlight({{parameter.backlight-active-high}}))"
bindings:
  spi:
    kind: spi-device
  dc:
    kind: digital-output
    initial: low
  reset:
    kind: digital-output
    initial: high
  backlight:
    kind: digital-output
    initial-from:
      parameter: backlight-active-high
      values: { "true": low, "false": high }
parameters:
  controller:
    type: enum
    values: [gc9a01]
    rust-values:
      gc9a01: "::barracuda_mipi_dbi_display::GC9A01"
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
    rust-values:
      rgb: "::barracuda_mipi_dbi_display::MipiDbiColorOrder::Rgb"
      bgr: "::barracuda_mipi_dbi_display::MipiDbiColorOrder::Bgr"
    default: bgr
  invert-colors:
    type: boolean
    default: false
  orientation:
    type: enum
    values: [deg0, deg90, deg180, deg270]
    rust-values:
      deg0: "::barracuda_driver::display::DisplayOrientation::Deg0"
      deg90: "::barracuda_driver::display::DisplayOrientation::Deg90"
      deg180: "::barracuda_driver::display::DisplayOrientation::Deg180"
      deg270: "::barracuda_driver::display::DisplayOrientation::Deg270"
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
    assert_valid_rust(&rust);

    assert!(rust.contains("controller_binding_type!(SPI2)"));
    assert!(rust.contains("binding_type!(GPIO7)"));
    assert!(rust.contains("MipiDbiDisplayDriver"));
    assert!(rust.contains("GC9A01"));
    assert!(rust.contains("Size::new(240, 240)"));
    assert!(rust.contains("with_inverted_colors(true)"));
    assert!(rust.contains("display::BuiltinDisplay for GeneratedBuiltins"));
    assert!(!rust.contains("dial-copy"));
}

#[test]
fn renders_a_driver_unknown_to_the_generator_from_its_manifest() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "future-sensor",
        r#"
id: future-sensor
api-version: 1
capability: environment-sensor
implementation:
  package: future-sensor
  crate: future_sensor
  factory: "::{{crate}}::FutureSensorDriver<{{binding.enable.type}}>"
  bindings-expression: "::{{crate}}::Bindings::new({{binding.enable.value}})"
  config-expression: "::{{crate}}::Config::new({{parameter.sample-rate}})"
bindings:
  enable:
    kind: digital-output
    initial: low
parameters:
  sample-rate:
    type: integer
    required: true
"#,
    );
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(
        r#"
name: future-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
builtin-peripherals:
  air-quality:
    driver: future-sensor
    bindings:
      enable: GPIO10
    parameters:
      sample-rate: 50
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved future Driver");

    let rust = render_board_hal(&board, &resolved).expect("generated future Driver");
    assert_valid_rust(&rust);

    assert!(rust.contains("FutureSensorDriver"));
    assert!(rust.contains("future_sensor::Bindings::new"));
    assert!(rust.contains("future_sensor::Config::new(50)"));
    assert!(rust.contains("take_air_quality"));
}

#[test]
fn renders_an_unknown_i2c_driver_through_the_selected_platform_hal() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "future-i2c-sensor",
        r#"
id: future-i2c-sensor
api-version: 1
capability: environment-sensor
implementation:
  package: future-i2c-sensor
  crate: future_i2c_sensor
  factory: "::{{crate}}::Driver<{{binding.i2c.type}}>"
  bindings-expression: "{{binding.i2c.value}}"
  config-expression: "::{{crate}}::Config::new({{parameter.address}})"
bindings:
  i2c:
    kind: i2c-device
parameters:
  address:
    type: integer
    required: true
"#,
    );
    let catalog = load_catalog(root.path()).expect("driver catalog");
    let board = parse(
        r#"
name: sensor-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
internal-io:
  i2c-device:
    environment:
      peripheral: I2C0
      scl: GPIO1
      sda: GPIO2
      frequency-hz: 400000
builtin-peripherals:
  environment:
    driver: future-i2c-sensor
    bindings:
      i2c: environment
    parameters:
      address: 118
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved I2C Driver");

    let rust = render_board_hal(&board, &resolved).expect("generated I2C Driver");
    assert_valid_rust(&rust);

    assert!(rust.contains("__platform::hal::controller_binding_type!(I2C0)"));
    assert!(rust.contains("__platform::hal::i2c_device"));
    assert!(rust.contains("future_i2c_sensor::Config::new(118)"));
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
