//! Filesystem-discovered Platform resolution behavior.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_platform_config::{
    discover_platforms, resolve_board_platform, resolve_platform, HalBinding, PlatformTarget,
};
use tempfile::tempdir;

fn add_platform(root: &Path, name: &str, chip: &str, target: &str) {
    let directory = root.join("platforms").join(name);
    let crate_name = name.replace('-', "_");
    fs::create_dir_all(&directory).expect("Platform directory");
    fs::write(
        directory.join("platform.yml"),
        format!(
            "name: {name}\ninfo:\n  family: acme\n  environment: bare-metal\npackage: barracuda-platform-{name}\ncrate: barracuda_platform_{crate_name}\ntype: AcmePlatform\nhal:\n  bindings: [digital-output, i2c-device]\n  runtime-i2c-controllers: [I2C0, I2C1]\n  runtime-spi-controllers: [SPI2]\n  runtime-uart-controllers: [UART1]\n  runtime-adc-controllers:\n    - controller: ADC1\n      channels:\n        - channel: CHANNEL0\n          pin: GPIO1\n  runtime-pwm-controllers:\n    - controller: PWM0\n      timers: [TIMER0]\n      channels: [CHANNEL0]\n  runtime-i2s-controllers:\n    - controller: I2S0\n      dma-channels: [DMA_CH0]\n      dma-buffer-bytes: 4096\nselection:\n  board-chips:\n    - '{chip}'\n  targets:\n    - triple: '{target}'\nsystem-image:\n  layout:\n    driver: command\n    program: tools/system-image\n    arguments: [describe, '{{layout}}']\n  flash:\n    driver: command\n    program: tools/system-image\n    arguments: [flash, '{{layout}}', '{{image}}']\napplication:\n  support-binaries: [acme-network]\n  launcher:\n    program: privilege-tool\n    arguments: ['{{support:acme-network}}', '{{application}}']\n"
        ),
    )
    .expect("Platform manifest");
}

#[test]
fn discovers_an_unknown_platform_entirely_from_its_own_directory() {
    let root = tempdir().expect("temporary workspace");
    add_platform(
        root.path(),
        "acme-rv",
        "acme123*",
        "riscv64acme-unknown-none-elf",
    );

    let board = resolve_board_platform(
        root.path(),
        "acme123-pro",
        Some("riscv64acme-unknown-none-elf"),
    )
    .expect("Platform selected from Board");
    let target = resolve_platform(
        root.path(),
        PlatformTarget::new("riscv64acme-unknown-none-elf", "none", "riscv64"),
        None,
    )
    .expect("Platform selected from Cargo target");

    assert_eq!(board.name(), "acme-rv");
    assert_eq!(board.info().family(), "acme");
    assert_eq!(board.info().environment(), "bare-metal");
    assert_eq!(board.package(), "barracuda-platform-acme-rv");
    assert_eq!(target.name(), board.name());
    assert_eq!(board.directory(), root.path().join("platforms/acme-rv"));
    assert_eq!(board.application().support_binaries(), ["acme-network"]);
    assert!(board.cargo_features_for_chip("acme123-pro").is_empty());
    assert!(board.hal().supports(HalBinding::DigitalOutput));
    assert!(board.hal().supports(HalBinding::I2cDevice));
    assert!(!board.hal().supports(HalBinding::SpiDevice));
    assert_eq!(board.hal().runtime_i2c_controllers(), ["I2C0", "I2C1"]);
    assert_eq!(board.hal().runtime_spi_controllers(), ["SPI2"]);
    assert_eq!(board.hal().runtime_uart_controllers(), ["UART1"]);
    let adc = &board.hal().runtime_adc_controllers()[0];
    assert_eq!(adc.controller(), "ADC1");
    assert_eq!(adc.channels()[0].channel(), "CHANNEL0");
    assert_eq!(adc.channels()[0].pin(), "GPIO1");
    let pwm = &board.hal().runtime_pwm_controllers()[0];
    assert_eq!(pwm.controller(), "PWM0");
    assert_eq!(pwm.timers(), ["TIMER0"]);
    assert_eq!(pwm.channels(), ["CHANNEL0"]);
    let i2s = &board.hal().runtime_i2s_controllers()[0];
    assert_eq!(i2s.controller(), "I2S0");
    assert_eq!(i2s.dma_channels(), ["DMA_CH0"]);
    assert_eq!(i2s.dma_buffer_bytes(), 4096);
    assert_eq!(
        board
            .application()
            .launcher()
            .map(|launcher| launcher.program()),
        Some(Path::new("privilege-tool"))
    );
}

#[test]
fn platform_declares_the_rustup_toolchain_for_ordinary_cargo_commands() {
    let root = tempdir().expect("temporary workspace");
    add_platform(
        root.path(),
        "acme-rv",
        "acme123",
        "riscv64acme-unknown-none-elf",
    );
    let manifest = root.path().join("platforms/acme-rv/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    fs::write(
        &manifest,
        yaml.replace(
            "selection:\n",
            "build:\n  rustup-toolchain: acme\n  build-std: [core, alloc]\n  environment-file: ~/export-acme.sh\n  missing-toolchain-prompt: |\n    Install the Acme Rust toolchain first.\n  linker-script: acme-link.x\nselection:\n",
        ),
    )
    .expect("Platform build policy");

    let platform =
        resolve_board_platform(root.path(), "acme123", Some("riscv64acme-unknown-none-elf"))
            .expect("Platform selected from Board");

    assert_eq!(platform.build().rustup_toolchain(), Some("acme"));
    assert_eq!(platform.build().build_std(), ["core", "alloc"]);
    assert_eq!(
        platform.build().environment_file(),
        Some(Path::new("~/export-acme.sh"))
    );
    assert_eq!(
        platform.build().missing_toolchain_prompt(),
        Some("Install the Acme Rust toolchain first.\n")
    );
    assert_eq!(platform.build().linker_script(), Some("acme-link.x"));
}

#[test]
fn rejects_a_linker_script_path() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "acme", "acme", "acme-none-elf");
    let manifest = root.path().join("platforms/acme/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    fs::write(
        &manifest,
        yaml.replace(
            "selection:\n",
            "build:\n  linker-script: ld/acme-link.x\nselection:\n",
        ),
    )
    .expect("Platform linker script");

    let error = discover_platforms(root.path()).expect_err("a path must fail");

    assert!(error.to_string().contains("linker-script"));
}

#[test]
fn rejects_an_incomplete_platform_toolchain_policy() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "acme", "acme", "acme-none-elf");
    let manifest = root.path().join("platforms/acme/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    fs::write(
        &manifest,
        yaml.replace(
            "selection:\n",
            "build:\n  rustup-toolchain: acme\nselection:\n",
        ),
    )
    .expect("incomplete Platform build policy");

    let error = discover_platforms(root.path()).expect_err("missing prompt must fail");

    assert!(error.to_string().contains("missing-toolchain-prompt"));
}

#[test]
fn platform_owns_cargo_features_for_supported_chips() {
    let root = tempdir().expect("temporary workspace");
    let directory = root.path().join("platforms/acme-rv");
    fs::create_dir_all(&directory).expect("Platform directory");
    fs::write(
        directory.join("platform.yml"),
        "name: acme-rv\ninfo:\n  family: acme\n  environment: bare-metal\npackage: barracuda-platform-acme-rv\ncrate: barracuda_platform_acme_rv\ntype: AcmePlatform\nselection:\n  board-chips: ['acme*']\n  targets:\n    - triple: 'riscv64acme-unknown-none-elf'\n  features-by-chip:\n    acme123: [acme123-hal]\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
    )
    .expect("Platform manifest");

    let platform =
        resolve_board_platform(root.path(), "acme123", Some("riscv64acme-unknown-none-elf"))
            .expect("Platform selected from Board");

    assert_eq!(platform.cargo_features_for_chip("acme123"), ["acme123-hal"]);
    assert!(platform.cargo_features_for_chip("acme999").is_empty());
}

#[test]
fn rejects_unbounded_runtime_i2s_storage() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "acme", "acme", "acme-none-elf");
    let manifest = root.path().join("platforms/acme/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    fs::write(
        &manifest,
        yaml.replace("dma-buffer-bytes: 4096", "dma-buffer-bytes: 0"),
    )
    .expect("invalid Platform manifest");

    let error = discover_platforms(root.path()).expect_err("zero DMA buffer must fail");
    assert!(error.to_string().contains("nonzero buffer"));
}

#[test]
fn an_sdmmc_platform_lists_the_sd_host_slots_it_hands_out() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "acme", "acme", "acme-none-elf");
    let manifest = root.path().join("platforms/acme/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    let with = |bindings: &str, slots: &str| {
        let yaml = yaml
            .replace(
                "bindings: [digital-output, i2c-device]",
                &format!("bindings: [{bindings}]"),
            )
            .replace("selection:\n", &format!("{slots}selection:\n"));
        fs::write(&manifest, yaml).expect("Platform manifest");
        discover_platforms(root.path())
    };

    let platforms = with(
        "sdmmc-device",
        "  sdmmc-slots:\n    - slot: 0\n      power: { channel: 4, millivolts: 3300 }\n",
    )
    .expect("SD host slots");
    let slot = platforms[0].hal().sdmmc_slot(0).expect("slot 0");
    let power = slot.power().expect("slot 0 supply");
    assert_eq!((power.channel(), power.millivolts()), (4, 3300));
    assert!(platforms[0].hal().sdmmc_slot(1).is_none());

    for (bindings, slots) in [
        // The binding without a slot to hand out.
        ("sdmmc-device", ""),
        // Slots without the binding that takes them.
        ("digital-output", "  sdmmc-slots:\n    - slot: 0\n"),
        // One slot twice.
        (
            "sdmmc-device",
            "  sdmmc-slots:\n    - slot: 0\n    - slot: 0\n",
        ),
        // A supply of no voltage.
        (
            "sdmmc-device",
            "  sdmmc-slots:\n    - slot: 0\n      power: { channel: 4, millivolts: 0 }\n",
        ),
    ] {
        let error = with(bindings, slots).expect_err("inconsistent SD host slots");
        assert!(error.to_string().contains("SD host slot"), "{error}");
    }
}

#[test]
fn the_esp32p4_keeps_slot_1_and_powers_slot_0_from_ldo_vo4() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let platforms = discover_platforms(&root).expect("workspace Platform catalog");
    let slots = |name: &str| {
        platforms
            .iter()
            .find(|platform| platform.name() == name)
            .expect("repository Platform")
            .hal()
            .sdmmc_slots()
            .to_vec()
    };

    let esp32p4 = slots("esp32p4");
    assert_eq!(esp32p4.len(), 1);
    assert_eq!(esp32p4[0].slot(), 0);
    let power = esp32p4[0].power().expect("LDO VO4");
    assert_eq!((power.channel(), power.millivolts()), (4, 3300));

    let esp32s3 = slots("esp32s3");
    assert_eq!(esp32s3.len(), 1);
    assert_eq!((esp32s3[0].slot(), esp32s3[0].power()), (0, None));
}

#[test]
fn rejects_a_platform_without_fixed_identity() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "acme", "acme", "acme-none-elf");
    let manifest = root.path().join("platforms/acme/platform.yml");
    let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
    fs::write(
        &manifest,
        yaml.replace("info:\n  family: acme\n  environment: bare-metal\n", ""),
    )
    .expect("Platform manifest without identity");

    let error = discover_platforms(root.path()).expect_err("missing identity must fail");
    assert!(error.to_string().contains("info"));
}

#[test]
fn legacy_application_source_is_rejected() {
    let root = tempdir().expect("temporary workspace");
    let directory = root.path().join("platforms/acme");
    fs::create_dir_all(&directory).expect("Platform directory");
    fs::write(
        directory.join("platform.yml"),
        "name: acme\ninfo:\n  family: acme\n  environment: hosted\npackage: barracuda-platform-acme\ncrate: barracuda_platform_acme\ntype: AcmePlatform\nselection:\n  board-chips: [acme]\n  targets:\n    - os: acme\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\napplication:\n  entry:\n    source: application.rs\n",
    )
    .expect("Platform manifest");

    let error = discover_platforms(root.path()).expect_err("entry source is no longer metadata");

    assert!(error.to_string().contains("entry"));
}

#[test]
fn rejects_missing_ambiguous_and_incompatible_platforms() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "first", "chip-*", "target-one");
    add_platform(root.path(), "second", "chip-*", "target-one");

    let error = resolve_board_platform(root.path(), "missing", None)
        .expect_err("missing Platform must fail");
    assert!(error.to_string().contains("no Platform"));

    let error = resolve_board_platform(root.path(), "chip-x", Some("target-one"))
        .expect_err("ambiguous Platform must fail");
    assert!(error.to_string().contains("multiple Platforms"));

    let target = PlatformTarget::new("target-one", "none", "custom");
    let error = resolve_platform(root.path(), target, Some("missing"))
        .expect_err("unknown requested Platform must fail");
    assert!(error.to_string().contains("missing"));
}

#[test]
fn central_resolver_contains_no_concrete_platform_registry() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let resolver = fs::read_to_string(root.join("platforms/config/src/lib.rs"))?;
    let selected = fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    for concrete in [
        "macos", "linux", "esp32", "esp32c3", "esp32c6", "esp32p4", "esp32s3", "stm32",
    ] {
        assert!(!resolver.contains(&format!("\"{concrete}\"")));
        assert!(!selected.contains(&format!("\"{concrete}\"")));
    }
    Ok(())
}

#[test]
fn esp32p4_does_not_advertise_an_unimplemented_ledc_pool() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let platforms = discover_platforms(&root).expect("workspace Platform catalog");
    let esp32p4 = platforms
        .iter()
        .find(|platform| platform.name() == "esp32p4")
        .expect("ESP32-P4 Platform");

    assert!(esp32p4.hal().runtime_pwm_controllers().is_empty());

    for name in ["esp32", "esp32c3", "esp32c6", "esp32s3"] {
        let platform = platforms
            .iter()
            .find(|platform| platform.name() == name)
            .expect("LEDC-capable ESP Platform");
        assert!(!platform.hal().runtime_pwm_controllers().is_empty());
    }
}

#[test]
fn every_repository_platform_declares_its_rustup_toolchain() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let platforms = discover_platforms(&root).expect("workspace Platform catalog");

    assert!(!platforms.is_empty());
    for platform in platforms {
        assert!(
            platform.build().rustup_toolchain().is_some(),
            "Platform `{}` must declare build.rustup-toolchain",
            platform.name()
        );
    }
}
