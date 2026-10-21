//! Implementation catalog behavior.

#![allow(clippy::expect_used)]

use std::fs;

use barracuda_board_config::{PeripheralParameter, parse};
use barracuda_peripheral_config::{
    GenerateError, ResolveError, load_catalog, render_board_hal, render_board_hal_for_platform,
    resolve_board, validate_platform_hal,
};
use barracuda_platform_config::{HalBinding, discover_platforms, resolve_board_platform};
use tempfile::tempdir;

const INDICATOR_MANIFEST: &str = r#"
id: indicator-led
api-version: 1
peripheral: indicator
implementation:
  package: barracuda-indicator-led
  crate: barracuda_indicator_led
  factory: "::{{crate}}::IndicatorLedImplementation<{{binding.pin.type}}>"
  bindings-expression: "{{binding.pin.value}}"
  config-expression: "::barracuda_peripheral::indicator::IndicatorConfig::new({{parameter.active-level}})"
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
      low: "::barracuda_peripheral::indicator::ActiveLevel::Low"
      high: "::barracuda_peripheral::indicator::ActiveLevel::High"
    required: true
"#;

const DISPLAY_MANIFEST: &str = r#"
id: test-display
api-version: 1
peripheral: display
implementation:
  package: test-display
  crate: test_display
  factory: "::{{crate}}::TestDisplayImplementation<{{binding.spi.type}}, {{binding.dc.type}}>"
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

fn board(implementation: &str, bindings: &str, parameters: &str) -> String {
    let bindings = if bindings.is_empty() {
        String::from("      bindings: {}\n")
    } else {
        format!(
            "      bindings:\n{}",
            bindings.replace("      ", "        ")
        )
    };
    let parameters = parameters.replace("      ", "        ");
    format!(
        "name: test-board\nhardware:\n  chip: test-chip\nnative-layout:\n  artifact: memory.x\nperipherals:\n  devices:\n    status-led:\n      implementation: {implementation}\n{bindings}      parameters:\n{parameters}"
    )
}

fn add_driver(root: &std::path::Path, directory: &str, manifest: &str) {
    let peripheral = manifest
        .lines()
        .find_map(|line| line.strip_prefix("peripheral: "))
        .expect("peripheral API");
    let path = root
        .join("peripherals/impl")
        .join(peripheral)
        .join(directory);
    fs::create_dir_all(&path).expect("implementation directory");
    fs::write(path.join("peripheral.yml"), manifest).expect("implementation manifest");
}

fn assert_valid_rust(source: &str) {
    syn::parse_file(source).expect("generated source is valid Rust");
}

#[test]
fn platform_runtime_controllers_are_composed_without_board_protocol_configuration() {
    let root = tempdir().expect("temporary workspace");
    fs::create_dir_all(root.path().join("implementations")).expect("implementation catalog");
    let platform_directory = root.path().join("platforms/acme");
    fs::create_dir_all(&platform_directory).expect("Platform directory");
    fs::write(
        platform_directory.join("platform.yml"),
        r#"
name: acme
info:
  family: acme
  environment: bare-metal
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
peripherals:
  io:
    i2c-device:
      peripheral-control:
        peripheral: I2C0
        scl: GPIO8
        sda: GPIO9
        frequency-hz: 400000
    i2s-stream:
      peripheral-audio:
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
    let catalog = load_catalog(root.path()).expect("empty Implementation catalog");
    let resolved = resolve_board(&board, &catalog).expect("empty peripheral composition");
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
    let catalog = load_catalog(&root).expect("repository Implementation catalog");
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
    assert!(rust.contains("hal::RuntimeIo<4, 2, 2, 1, 1, 1, 1>"));
    assert!(rust.contains("controller_binding_type!(I2C0)"));
    assert!(rust.contains("controller_binding_type!(I2C1)"));
    assert!(rust.contains("controller_binding_type!(SPI2)"));
    assert!(rust.contains("controller_binding_type!(SPI3)"));
    assert!(rust.contains("controller_binding_type!(UART1)"));
    assert!(rust.contains("controller_binding_type!(ADC1)"));
    assert!(rust.contains("runtime_adc_channel!(ADC1_CH0, GPIO1)"));
    assert!(rust.contains("controller_binding_type!(LEDC)"));
    assert!(rust.contains("runtime_pwm_timer!(Timer0)"));
    assert!(rust.contains("runtime_pwm_channel!(Channel0)"));
    assert!(rust.contains("controller_binding_type!(I2S0)"));
    assert!(rust.contains("controller_binding_type!(DMA_CH0)"));
    assert!(rust.contains("runtime_i2s_dma_buffers!(4096)"));
}

#[test]
fn resolves_a_board_against_implementation_owned_schemas() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(&board(
        "indicator-led",
        "      pin: PB0\n",
        "      active-level: high\n",
    ))
    .expect("Board YAML");

    let resolved = resolve_board(&board, &catalog).expect("resolved composition");
    let peripheral = resolved.peripheral("status-led").expect("status LED");

    assert_eq!(peripheral.implementation().id(), "indicator-led");
    assert_eq!(peripheral.implementation().peripheral(), "indicator");
    assert_eq!(peripheral.binding("pin"), Some("PB0"));
    assert_eq!(
        peripheral.parameter("active-level"),
        Some(&PeripheralParameter::String(String::from("high")))
    );
}

#[test]
fn rejects_an_unknown_implementation_id() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(&board(
        "typo-implementation",
        "      pin: PB0\n",
        "      active-level: high\n",
    ))
    .expect("Board YAML");

    assert!(matches!(
        resolve_board(&board, &catalog),
        Err(ResolveError::UnknownImplementation { .. })
    ));
}

#[test]
fn rejects_missing_and_unknown_bindings() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");

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
    let catalog = load_catalog(root.path()).expect("implementation catalog");

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
fn resolves_spi_devices_through_board_peripheral_io() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-display", DISPLAY_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let valid = parse(
        r#"
name: display-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  io:
    spi-device:
      display:
        peripheral: SPI2
        sck: GPIO6
        mosi: GPIO5
        miso: GPIO3
        chip-select: GPIO7
        frequency-hz: 40000000
  devices:
    display:
      implementation: test-display
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

    let three_wire = parse(
        r#"
name: display-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  io:
    spi-device:
      display:
        peripheral: SPI3
        sck: GPIO17
        mosi: GPIO21
        three-wire: true
        chip-select: GPIO15
        frequency-hz: 40000000
  devices:
    display:
      implementation: test-display
      bindings:
        spi: display
        dc: GPIO33
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&three_wire, &catalog).expect("resolved three-wire device");
    let rust = render_board_hal(&three_wire, &resolved).expect("generated three-wire device");
    assert!(rust.contains("hal::spi_device_three_wire"));
    assert!(rust.contains("hal::ThreeWireSpiDevice"));
    assert_valid_rust(&rust);

    let invalid = parse(
        r#"
name: display-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  devices:
    display:
      implementation: test-display
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
fn renders_media_data_planes_without_implementation_specific_generator_code() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "test-media",
        r#"
id: test-media
api-version: 1
peripheral: camera
implementation:
  package: test-media
  crate: test_media
  factory: "::{{crate}}::Implementation<{{binding.pixels.type}}, {{binding.camera.type}}, {{binding.audio.type}}>"
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
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: media-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  io:
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
  devices:
    media:
      implementation: test-media
      bindings: { pixels: pixels, camera: camera, audio: audio }
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved media Implementation");
    let rust = render_board_hal(&board, &resolved).expect("generated media HAL");

    assert_valid_rust(&rust);
    assert!(rust.contains("hal::spi_bus("));
    assert!(rust.contains("hal::camera_dma_buffer!(98304)"));
    assert!(rust.contains("hal::camera_capture("));
    assert!(rust.contains("hal::i2s_dma_buffers!(1024)"));
    assert!(rust.contains("hal::i2s_stream("));
    assert!(rust.contains("test_media::Implementation"));
}

const PARALLEL_MANIFEST: &str = r#"
id: test-parallel
api-version: 1
peripheral: display
implementation:
  package: test-parallel
  crate: test_parallel
  factory: "::{{crate}}::Implementation<{{binding.bus.type}}>"
  bindings-expression: "::{{crate}}::Bindings::new({{binding.bus.value}})"
  config-expression: "()"
bindings:
  bus:
    kind: parallel-output
parameters: {}
"#;

#[test]
fn renders_a_parallel_output_with_data_command() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-parallel", PARALLEL_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: parallel-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  io:
    parallel-output:
      lcd:
        peripheral: LCD_CAM
        dma: DMA_CH1
        wr: GPIO11
        dc: GPIO12
        chip-select: GPIO14
        data: [GPIO13, GPIO9, GPIO8, GPIO7, GPIO6, GPIO5, GPIO4, GPIO3]
        frequency-hz: 10000000
        dma-buffer-bytes: 4096
  devices:
    display:
      implementation: test-parallel
      bindings: { bus: lcd }
"#,
    )
    .expect("Board YAML");
    assert!(board.peripheral_io().uses_controller("LCD_CAM"));
    assert!(board.peripheral_io().uses_dma("DMA_CH1"));
    let resolved = resolve_board(&board, &catalog).expect("resolved parallel Implementation");
    let rust = render_board_hal(&board, &resolved).expect("generated parallel HAL");

    assert_valid_rust(&rust);
    assert!(rust.contains("hal::parallel_output_dma_buffer!(4096)"));
    assert!(rust.contains("hal::parallel_output(bindings.display_bus_controller, bindings.display_bus_dma, bindings.display_bus_wr, bindings.display_bus_dc, bindings.display_bus_cs, bindings.display_bus_d0,"));
    assert!(rust.contains("hal::ParallelOutputBus"));
    assert!(rust.contains("pin_binding_type!(GPIO12)"));
    assert!(rust.contains("pin_binding_type!(GPIO13)"));
}

#[test]
fn renders_a_data_only_parallel_output_with_its_static_dma_buffer() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-parallel", PARALLEL_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: panel-board
hardware:
  chip: test-chip
native-layout:
  artifact: memory.x
peripherals:
  io:
    parallel-output:
      source:
        peripheral: LCD_CAM
        dma: DMA_CH1
        wr: GPIO16
        chip-select: GPIO13
        data: [GPIO6, GPIO14, GPIO7, GPIO12, GPIO9, GPIO11, GPIO8, GPIO10]
        frequency-hz: 16000000
        dma-buffer-bytes: 256
  devices:
    panel:
      implementation: test-parallel
      bindings: { bus: source }
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved panel Implementation");
    let rust = render_board_hal(&board, &resolved).expect("generated panel HAL");

    assert_valid_rust(&rust);
    assert!(rust.contains("hal::parallel_output_dma_buffer!(256)"));
    assert!(rust.contains("hal::parallel_output_data_only(bindings.panel_bus_controller, bindings.panel_bus_dma, bindings.panel_bus_wr, bindings.panel_bus_cs, bindings.panel_bus_d0,"));
    assert!(!rust.contains("panel_bus_dc"));
    assert!(rust.contains("hal::ParallelOutputBus"));
    assert!(rust.contains("controller_binding_type!(DMA_CH1)"));
    assert!(rust.contains("pin_binding_type!(GPIO10)"));
}

#[test]
fn rejects_duplicate_ids_and_directory_id_mismatches() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "wrong-directory", INDICATOR_MANIFEST);

    assert!(load_catalog(root.path()).is_err());
}

#[test]
fn rejects_a_manifest_in_the_wrong_peripheral_api_directory() {
    let root = tempdir().expect("temporary workspace");
    let path = root.path().join("peripherals/impl/display/indicator-led");
    fs::create_dir_all(&path).expect("implementation directory");
    fs::write(path.join("peripheral.yml"), INDICATOR_MANIFEST).expect("implementation manifest");

    let error = load_catalog(root.path()).expect_err("mismatched peripheral API directory");

    assert!(
        error
            .to_string()
            .contains("does not match implementation category")
    );
}

#[test]
fn rejects_unknown_implementation_template_placeholders_during_catalog_load() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "invalid-implementation",
        r#"
id: invalid-implementation
api-version: 1
peripheral: indicator
implementation:
  package: invalid-implementation
  crate: invalid_driver
  factory: "::{{crate}}::Implementation<{{binding.typo.type}}>"
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
fn renders_a_static_hal_from_only_board_and_implementation_yaml() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: nucleo-copy
hardware:
  chip: stm32u5a5zj
native-layout:
  artifact: memory.x
peripherals:
  devices:
    status-led:
      implementation: indicator-led
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
    assert!(rust.contains("IndicatorLedImplementation"));
    assert!(rust.contains("PeripheralImplementation>::initialize"));
    assert!(rust.contains("indicator::IndicatorPeripheral for GeneratedPeripherals"));
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
peripheral: display
implementation:
  package: barracuda-mipi-dbi-display
  crate: barracuda_mipi_dbi_display
  factory: "::{{crate}}::MipiDbiDisplayImplementation<{{binding.spi.type}}, {{binding.dc.type}}, {{parameter.controller}}, {{binding.reset.type}}, {{binding.backlight.type}}, {{hal.delay.type}}, 512>"
  bindings-expression: "::{{crate}}::MipiDbiDisplayBindings::new({{binding.spi.value}}, {{binding.dc.value}}, {{binding.reset.value}}, Some({{binding.backlight.value}}), {{hal.delay.value}}, ::{{crate}}::take_transfer_buffer())"
  config-expression: "::{{crate}}::MipiDbiDriverConfig::new({{parameter.controller}}, ::{{crate}}::MipiDbiDisplayConfig::new(::{{crate}}::Size::new({{parameter.width}}, {{parameter.height}}), ::barracuda_peripheral::display::PixelFormat::Rgb565, {{parameter.orientation}}).with_offset({{parameter.offset-x}}, {{parameter.offset-y}}).with_color_order({{parameter.color-order}}).with_inverted_colors({{parameter.invert-colors}}).with_digital_backlight({{parameter.backlight-active-high}}))"
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
      deg0: "::barracuda_peripheral::display::DisplayOrientation::Deg0"
      deg90: "::barracuda_peripheral::display::DisplayOrientation::Deg90"
      deg180: "::barracuda_peripheral::display::DisplayOrientation::Deg180"
      deg270: "::barracuda_peripheral::display::DisplayOrientation::Deg270"
    default: deg0
  backlight-active-high:
    type: boolean
    default: true
"#,
    );
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: dial-copy
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
peripherals:
  io:
    spi-device:
      display-spi:
        peripheral: SPI2
        sck: GPIO6
        mosi: GPIO5
        chip-select: GPIO7
        frequency-hz: 80000000
  devices:
    display:
      implementation: mipi-dbi-display
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
    assert!(rust.contains("MipiDbiDisplayImplementation"));
    assert!(rust.contains("GC9A01"));
    assert!(rust.contains("Size::new(240, 240)"));
    assert!(rust.contains("with_inverted_colors(true)"));
    assert!(rust.contains("display::DisplayPeripheral for GeneratedPeripherals"));
    assert!(!rust.contains("dial-copy"));
}

#[test]
fn rejects_an_implementation_without_a_registered_peripheral_api() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "future-sensor",
        r#"
id: future-sensor
api-version: 1
peripheral: environment-sensor
implementation:
  package: future-sensor
  crate: future_sensor
  factory: "::{{crate}}::FutureSensorImplementation<{{binding.enable.type}}>"
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
    let error = load_catalog(root.path()).expect_err("unregistered peripheral API");

    assert!(
        error
            .to_string()
            .contains("add the API trait before its implementation")
    );
}

#[test]
fn renders_an_i2c_peripheral_implementation_through_the_selected_platform_hal() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "future-i2c-sensor",
        r#"
id: future-i2c-sensor
api-version: 1
peripheral: power-monitor
implementation:
  package: future-i2c-sensor
  crate: future_i2c_sensor
  factory: "::{{crate}}::Implementation<{{binding.i2c.type}}>"
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
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: sensor-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
peripherals:
  io:
    i2c-device:
      environment:
        peripheral: I2C0
        scl: GPIO1
        sda: GPIO2
        frequency-hz: 400000
  devices:
    environment:
      implementation: future-i2c-sensor
      bindings:
        i2c: environment
      parameters:
        address: 118
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved I2C Implementation");

    let rust = render_board_hal(&board, &resolved).expect("generated I2C Implementation");
    assert_valid_rust(&rust);

    assert!(rust.contains("__platform::hal::controller_binding_type!(I2C0)"));
    assert!(rust.contains("__platform::hal::i2c_device"));
    assert!(rust.contains("__platform::hal::I2cDevice"));
    assert!(rust.contains("shared_i2c_environment.device()"));
    assert!(rust.contains("future_i2c_sensor::Config::new(118)"));
}

#[test]
fn renders_a_gpio_binding_as_a_configurable_platform_pin() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "strapped-touch",
        r#"
id: strapped-touch
api-version: 1
peripheral: touch
implementation:
  package: strapped-touch
  crate: strapped_touch
  factory: "::{{crate}}::Implementation<{{binding.strap.type}}>"
  bindings-expression: "{{binding.strap.value}}"
  config-expression: "()"
bindings:
  strap:
    kind: gpio
parameters: {}
"#,
    );
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: strap-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
peripherals:
  devices:
    touch:
      implementation: strapped-touch
      bindings:
        strap: GPIO23
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved gpio binding");

    let rust = render_board_hal(&board, &resolved).expect("generated gpio binding");
    assert_valid_rust(&rust);
    assert!(rust.contains("__platform::hal::pin_binding_type!(GPIO23)"));
    assert!(rust.contains("strapped_touch::Implementation<::barracuda_platform_selected::__platform::hal::ConfigurablePin>"));
    assert!(rust.contains("__platform::hal::configurable_pin(bindings.touch_strap)"));
}

#[test]
fn multiple_i2c_peripherals_share_one_static_bus_owner() {
    let root = tempdir().expect("temporary workspace");
    add_driver(
        root.path(),
        "future-power-monitor",
        r#"
id: future-power-monitor
api-version: 1
peripheral: power-monitor
implementation:
  package: future-power-monitor
  crate: future_power_monitor
  factory: "::{{crate}}::Implementation<{{binding.i2c.type}}>"
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
    add_driver(
        root.path(),
        "future-real-time-clock",
        r#"
id: future-real-time-clock
api-version: 1
peripheral: real-time-clock
implementation:
  package: future-real-time-clock
  crate: future_real_time_clock
  factory: "::{{crate}}::Implementation<{{binding.i2c.type}}>"
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
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        r#"
name: shared-sensor-board
hardware:
  chip: esp32s3
native-layout:
  artifact: partitions.csv
peripherals:
  io:
    i2c-device:
      internal-sensors:
        peripheral: I2C0
        scl: GPIO1
        sda: GPIO2
        frequency-hz: 400000
  devices:
    power-monitor:
      implementation: future-power-monitor
      bindings: { i2c: internal-sensors }
      parameters: { address: 24 }
    real-time-clock:
      implementation: future-real-time-clock
      bindings: { i2c: internal-sensors }
      parameters: { address: 105 }
"#,
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved shared I2C Implementations");

    let rust = render_board_hal(&board, &resolved).expect("generated shared I2C Implementations");
    assert_valid_rust(&rust);

    assert_eq!(rust.matches("controller_binding_type!(I2C0)").count(), 2);
    assert_eq!(rust.matches("i2c_bus_manager!(").count(), 1);
    assert_eq!(
        rust.matches("shared_i2c_internal_sensors.device()").count(),
        2
    );
}

const DSI_DISPLAY_MANIFEST: &str = r#"
id: test-dsi-display
api-version: 1
peripheral: display
implementation:
  package: test-dsi-display
  crate: test_dsi_display
  factory: "::{{crate}}::Implementation<{{binding.host.type}}>"
  bindings-expression: "{{binding.host.value}}"
  config-expression: "()"
bindings:
  host:
    kind: dsi-host
parameters: {}
"#;

fn dsi_board(backlight: &str) -> String {
    format!(
        r#"
name: dsi-board
hardware:
  chip: esp32p4
native-layout:
  artifact: partitions.csv
peripherals:
  io:
    dsi-host:
      panel:
        peripheral: MIPI_DSI
        dma: VDMA_CH0
{backlight}        data-lanes: 2
        phy-power-channel: 3
        phy-power-millivolts: 2500
  devices:
    screen:
      implementation: test-dsi-display
      bindings: {{ host: panel }}
"#
    )
}

#[test]
fn renders_a_dsi_host_with_or_without_a_backlight_pin() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-dsi-display", DSI_DISPLAY_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let render = |backlight: &str| {
        let board = parse(&dsi_board(backlight)).expect("DSI Board YAML");
        let resolved = resolve_board(&board, &catalog).expect("resolved DSI display");
        let rust = render_board_hal(&board, &resolved).expect("generated DSI HAL");
        assert_valid_rust(&rust);
        assert!(rust.contains("controller_binding_type!(MIPI_DSI)"));
        assert!(rust.contains("controller_binding_type!(VDMA_CH0)"));
        assert!(rust.contains(", 2, 3, 2500)"));
        rust
    };

    let pin = render("        backlight: GPIO22\n");
    assert!(pin.contains("pin_binding_type!(GPIO22)"));
    assert!(pin.contains("hal::dsi_backlight_pin(bindings.screen_host_backlight)"));

    let external = render("");
    assert!(!external.contains("GPIO22"));
    assert!(external.contains("hal::dsi_backlight_none()"));
}

#[test]
fn only_a_platform_with_rmii_ethernet_accepts_a_wired_board() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = load_catalog(&root).expect("repository Implementation catalog");
    let wired = |chip: &str| {
        format!(
            r#"
name: wired-board
hardware:
  chip: {chip}
  ethernet:
    phy: {{ model: ip101gri, address: 1 }}
    rmii:
      reference-clock: {{ source: external, input: GPIO50 }}
      mdc: GPIO31
      mdio: GPIO52
      tx-en: GPIO49
      txd0: GPIO34
      txd1: GPIO35
      crs-dv: GPIO28
      rxd0: GPIO29
      rxd1: GPIO30
native-layout:
  artifact: partitions.csv
"#
        )
    };
    let validate = |chip: &str| {
        let board = parse(&wired(chip)).expect("wired Board YAML");
        let resolved = resolve_board(&board, &catalog).expect("resolved wired Board");
        let platform = resolve_board_platform(&root, chip, None).expect("repository Platform");
        validate_platform_hal(&board, &resolved, &platform)
    };

    assert_eq!(validate("esp32p4"), Ok(()));
    assert!(matches!(
        validate("esp32s3"),
        Err(GenerateError::UnsupportedHalBinding {
            binding: HalBinding::EthernetRmii,
            ..
        })
    ));
}

#[test]
fn only_a_platform_with_sdio_coprocessors_accepts_a_wifi_coprocessor_board() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = load_catalog(&root).expect("repository Implementation catalog");
    let board = |chip: &str| {
        format!(
            r#"
name: coprocessor-board
hardware:
  chip: {chip}
  wifi-coprocessor:
    chip: esp32c6
    firmware: esp-hosted-mcu
    sdio: {{ slot: 1, clk: GPIO12, cmd: GPIO13, d0: GPIO11, d1: GPIO10, d2: GPIO9, d3: GPIO8, frequency-hz: 40000000 }}
    reset: GPIO15
native-layout:
  artifact: partitions.csv
"#
        )
    };
    let validate = |chip: &str| {
        let board = parse(&board(chip)).expect("co-processor Board YAML");
        let resolved = resolve_board(&board, &catalog).expect("resolved co-processor Board");
        let platform = resolve_board_platform(&root, chip, None).expect("repository Platform");
        validate_platform_hal(&board, &resolved, &platform)
    };

    assert_eq!(validate("esp32p4"), Ok(()));
    assert!(matches!(
        validate("esp32s3"),
        Err(GenerateError::UnsupportedHalBinding {
            binding: HalBinding::WifiCoprocessorSdio,
            ..
        })
    ));
}

/// A microSD card on `slot` of the SD host, with `power` lines when any.
fn card_board(chip: &str, slot: u8, width: &str, power: &str) -> String {
    let data = if width == "4" {
        "[GPIO39, GPIO40, GPIO41, GPIO42]"
    } else {
        "[GPIO39]"
    };
    format!(
        r#"
name: card-board
hardware:
  chip: {chip}
native-layout:
  artifact: partitions.csv
peripherals:
  io:
    sdmmc-device:
      micro-sd:
        peripheral: SDHOST
        slot: {slot}
        clk: GPIO43
        cmd: GPIO44
        data: {data}
        bus-width: {width}
{power}  devices:
    micro-sd:
      implementation: sdmmc-fat-removable-storage
      bindings: {{ device: micro-sd }}
"#
    )
}

const VO4: &str = "        power-channel: 4\n        power-millivolts: 3300\n";

#[test]
fn an_sdmmc_card_takes_its_slot_through_the_platform_hand_off() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = load_catalog(&root).expect("repository Implementation catalog");
    let board = parse(&card_board("esp32p4", 0, "4", VO4)).expect("card Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved card Board");
    let platform = resolve_board_platform(&root, "esp32p4", None).expect("repository Platform");
    assert_eq!(validate_platform_hal(&board, &resolved, &platform), Ok(()));
    let rust = render_board_hal_for_platform(&board, &resolved, &platform).expect("card HAL");
    assert_valid_rust(&rust);

    // The binding's type and value are the Platform's: the value may take
    // the slot from the hand-off the entry passes beside the peripherals.
    assert!(rust.contains(
        "micro_sd_device_slot: ::barracuda_platform_selected::__platform::hal::sdmmc_slot_binding_type!(SDHOST, 0),"
    ));
    assert!(rust.contains(
        "$crate::__platform_selected::__platform::hal::sdmmc_slot_binding!($peripherals, $handoff, SDHOST, 0),"
    ));
    assert!(!rust.contains("$peripherals.SDHOST"));
    assert!(rust.contains("$peripherals.GPIO43,"));
    assert!(rust.contains(
        "($peripherals:ident) => {\n        $crate::__barracuda_generated_board_bindings!($peripherals, ())\n    };"
    ));
    assert!(rust.contains(
        "hal::sdmmc_device(bindings.micro_sd_device_slot, bindings.micro_sd_device_clk, bindings.micro_sd_device_cmd, bindings.micro_sd_device_d0, bindings.micro_sd_device_d1, bindings.micro_sd_device_d2, bindings.micro_sd_device_d3, 4, 4, 3300, 25000000)"
    ));

    // The ESP32-S3 keeps building its own controller for a one-bit card.
    let board = parse(&card_board("esp32s3", 0, "1", "")).expect("ESP32-S3 card Board");
    let resolved = resolve_board(&board, &catalog).expect("resolved ESP32-S3 card");
    let platform = resolve_board_platform(&root, "esp32s3", None).expect("ESP32-S3 Platform");
    assert_eq!(validate_platform_hal(&board, &resolved, &platform), Ok(()));
    let rust = render_board_hal_for_platform(&board, &resolved, &platform).expect("S3 card HAL");
    assert!(rust.contains(
        "hal::sdmmc_device_1bit(bindings.micro_sd_device_slot, bindings.micro_sd_device_clk, bindings.micro_sd_device_cmd, bindings.micro_sd_device_d0, 25000000)"
    ));
}

#[test]
fn a_card_needs_a_slot_and_supply_its_platform_hands_out() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = load_catalog(&root).expect("repository Implementation catalog");
    let validate = |chip: &str, yaml: &str| {
        let board = parse(yaml).expect("card Board YAML");
        let resolved = resolve_board(&board, &catalog).expect("resolved card Board");
        let platform = resolve_board_platform(&root, chip, None).expect("repository Platform");
        validate_platform_hal(&board, &resolved, &platform)
    };

    // The ESP32-P4 keeps slot 1 for a co-processor and hands out slot 0.
    assert_eq!(
        validate("esp32p4", &card_board("esp32p4", 1, "4", VO4)),
        Err(GenerateError::UnsupportedSdmmcSlot {
            platform: String::from("esp32p4"),
            resource: String::from("micro-sd"),
            slot: 1,
        })
    );
    // Slot 0's pads run from LDO VO4 at 3.3 V; a Board says so.
    for power in [
        "",
        "        power-channel: 4\n        power-millivolts: 1800\n",
        "        power-channel: 3\n        power-millivolts: 3300\n",
    ] {
        assert!(matches!(
            validate("esp32p4", &card_board("esp32p4", 0, "1", power)),
            Err(GenerateError::SdmmcSlotPower { slot: 0, .. })
        ));
    }
    assert_eq!(
        validate("esp32p4", &card_board("esp32p4", 0, "1", VO4)),
        Ok(())
    );
    // The ESP32-S3 powers no SD pads itself.
    assert!(matches!(
        validate("esp32s3", &card_board("esp32s3", 0, "1", VO4)),
        Err(GenerateError::SdmmcSlotPower { .. })
    ));
    // A Platform without the binding refuses the card outright.
    assert!(matches!(
        validate("esp32c6", &card_board("esp32c6", 0, "1", "")),
        Err(GenerateError::UnsupportedHalBinding {
            binding: HalBinding::SdmmcDevice,
            ..
        })
    ));
}

#[test]
fn every_repository_peripheral_composition_resolves_and_generates() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let catalog = load_catalog(root).expect("repository Implementation catalog");
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
        let platform = resolve_board_platform(
            root,
            board.hardware().chip(),
            board.toolchain().map(|toolchain| toolchain.target()),
        )
        .expect("repository Board Platform resolves");
        render_board_hal_for_platform(&board, &resolved, &platform)
            .expect("repository Board HAL generates");
    }
}

#[test]
fn atom_matrix_reserves_a_platform_spi_controller_without_a_fake_clock_pin() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let catalog = load_catalog(root).expect("repository Implementation catalog");
    let yaml = fs::read_to_string(root.join("boards/configs/m5stack-atom-matrix/board.yml"))
        .expect("Atom Matrix Board YAML");
    let board = parse(&yaml).expect("valid Atom Matrix Board");
    let resolved = resolve_board(&board, &catalog).expect("Atom Matrix resolves");
    let platform = resolve_board_platform(
        root,
        board.hardware().chip(),
        board.toolchain().map(|toolchain| toolchain.target()),
    )
    .expect("ESP32 Platform resolves");

    let rust = render_board_hal_for_platform(&board, &resolved, &platform)
        .expect("Atom Matrix HAL generates");
    assert_valid_rust(&rust);
    assert!(rust.contains("controller_binding_type!(SPI2)"));
    assert!(rust.contains("pin_binding_type!(GPIO27)"));
    assert!(rust.contains("hal::spi_output("));
    assert!(!rust.contains("led_strip_spi_sck"));
}

#[test]
fn host_boards_declare_their_peripherals_to_a_hal_with_device_models() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let catalog = load_catalog(root).expect("repository Implementation catalog");
    for name in ["local-linux", "local-macos", "espressif-esp-vocat-v1-2"] {
        let yaml = fs::read_to_string(root.join("boards/configs").join(name).join("board.yml"))
            .expect("Board YAML");
        let board = parse(&yaml).expect("valid Board YAML");
        let resolved = resolve_board(&board, &catalog).expect("repository Board resolves");
        let platform = resolve_board_platform(
            root,
            board.hardware().chip(),
            board.toolchain().map(|toolchain| toolchain.target()),
        )
        .expect("repository Board Platform resolves");
        let rust = render_board_hal_for_platform(&board, &resolved, &platform)
            .expect("repository Board HAL generates");
        assert_valid_rust(&rust);

        if !platform.hal().peripheral_models() {
            assert!(!rust.contains("declare_peripheral"), "{name}");
            continue;
        }
        assert!(rust.contains(
            r#"hal::declare_peripheral(&::barracuda_platform_selected::__platform::hal::PeripheralDeclaration { name: "real-time-clock", implementation: "rx8130ce-rtc", bindings: &[("i2c", "I2C2")], parameters: &[("address", "50")] });"#
        ));
        assert!(rust.contains(
            r#"PeripheralDeclaration { name: "power-monitor", implementation: "ina226-power-monitor", bindings: &[("i2c", "I2C2")], parameters: &[("address", "64"), ("shunt-micro-ohms", "5000")] }"#
        ));
        // A QSPI panel names its controller and the pins its model watches.
        assert!(
            rust.contains(
                r#"implementation: "st77916-qspi-display", bindings: &[("backlight", "GPIO15"), ("bus", "SPI0"), ("dc", "GPIO13"), ("power_enable", "GPIO20"), ("reset", "GPIO14")]"#
            ),
            "{name}"
        );
        let last_declaration = rust.rfind("declare_peripheral").expect("declarations");
        let first_initialization = rust.find("::initialize(").expect("initializations");
        assert!(last_declaration < first_initialization, "{name}");
        // The peripherals' controller leaves the runtime I2C pool.
        assert!(
            rust.contains("hal::RuntimeIo<8, 2, 0, 0, 0, 0, 0>"),
            "{name}"
        );
    }
}

const PANEL_MANIFEST: &str = r#"
id: test-panel
api-version: 1
peripheral: display
implementation:
  package: test-panel
  crate: test_panel
  factory: "::{{crate}}::TestPanelImplementation<{{binding.enable.type}}>"
  bindings-expression: "{{binding.enable.value}}"
  config-expression: "()"
bindings:
  enable:
    kind: digital-output
    initial: low
parameters: {}
"#;

fn two_device_board(implementation: &str, primary: &str) -> String {
    format!(
        "name: test-board\nhardware:\n  chip: stm32u5a5zj\nnative-layout:\n  artifact: memory.x\nperipherals:\n  devices:\n    front:\n      implementation: {implementation}\n{primary}      bindings:\n        enable: PB0\n    back:\n      implementation: {implementation}\n      bindings:\n        enable: PB1\n"
    )
}

#[test]
fn several_displays_render_one_enum_taken_by_instance() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "test-panel", PANEL_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board =
        parse(&two_device_board("test-panel", "      primary: true\n")).expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved composition");

    let rust = render_board_hal(&board, &resolved).expect("generated HAL");
    assert_valid_rust(&rust);
    assert!(rust.contains("::barracuda_board_hal::display_set!"));
    assert!(rust.contains("type Display = BoardDisplay;"));
    // Instances follow the Board's device names: `back` is 0, `front` is 1.
    assert!(rust.contains("0 => self.back.take().map(BoardDisplay::Back),"));
    assert!(rust.contains("1 => self.front.take().map(BoardDisplay::Front),"));

    let unmarked = parse(&two_device_board("test-panel", "")).expect("Board YAML");
    let resolved = resolve_board(&unmarked, &catalog).expect("resolved composition");
    let error = render_board_hal(&unmarked, &resolved).expect_err("ambiguous primary display");
    assert!(
        error
            .to_string()
            .contains("mark exactly one `primary: true`"),
        "{error}"
    );
}

#[test]
fn single_instance_peripherals_reject_a_second_device() {
    let root = tempdir().expect("temporary workspace");
    add_driver(root.path(), "indicator-led", INDICATOR_MANIFEST);
    let catalog = load_catalog(root.path()).expect("implementation catalog");
    let board = parse(
        &two_device_board("indicator-led", "")
            .replace("enable:", "pin:")
            .replace(
                "        pin: PB0\n",
                "        pin: PB0\n      parameters:\n        active-level: high\n",
            )
            .replace(
                "        pin: PB1\n",
                "        pin: PB1\n      parameters:\n        active-level: high\n",
            ),
    )
    .expect("Board YAML");
    let resolved = resolve_board(&board, &catalog).expect("resolved composition");
    let error = render_board_hal(&board, &resolved).expect_err("two indicators");
    assert!(
        error
            .to_string()
            .contains("only `display` supports several"),
        "{error}"
    );
}
