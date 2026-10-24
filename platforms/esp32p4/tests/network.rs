//! Projection of Board-declared network links into the entry's tokens.

#![allow(clippy::expect_used, missing_docs)]

#[path = "../build_network.rs"]
mod network;

use std::{fs, path::Path};

use barracuda_board_config::parse;

fn board(id: &str) -> barracuda_board_config::BoardDefinition {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../boards/configs")
        .join(id)
        .join("board.yml");
    parse(&fs::read_to_string(path).expect("repository Board")).expect("valid Board")
}

#[test]
fn a_wired_board_hands_the_entry_its_emac_and_rmii_pins() {
    for id in ["esp32p4-function-ev-board", "m5stack-unit-poep4"] {
        let board = board(id);
        let ethernet = board.hardware().ethernet().expect("wired Ethernet");
        let rust = network::network_tokens(Some(ethernet), None).expect("projected tokens");

        // The entry moves the EMAC, the clock input, the eight RMII and
        // management pins, then the PHY reset out of the chip peripherals.
        assert!(rust.contains(
            "$crate::application::NetworkTokens::new($peripherals.ETH, $peripherals.GPIO50, \
             $peripherals.GPIO29, $peripherals.GPIO30, $peripherals.GPIO28, $peripherals.GPIO34, \
             $peripherals.GPIO35, $peripherals.GPIO49, $peripherals.GPIO31, $peripherals.GPIO52, \
             $peripherals.GPIO51)"
        ));
        assert!(rust.contains(
            "::esp_hal::ethernet::clock::ExternalRefClock<::esp_hal::peripherals::GPIO50<'static>>"
        ));
        assert!(
            rust.contains("clock: ::esp_hal::ethernet::clock::ExternalRefClock::new(clock_input)")
        );
        assert!(rust.contains("phy_reset: Some(::esp_hal::gpio::Pin::degrade(phy_reset))"));
        assert!(rust.contains("phy_address: 1,"));
        assert!(rust.contains("macro_rules! __barracuda_esp32p4_network_tokens"));
    }
}

#[test]
fn an_internal_reference_clock_takes_its_output_pin_too() {
    let yaml = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../boards/configs/esp32p4-function-ev-board/board.yml"),
    )
    .expect("repository Board")
    .replacen(
        "source: external\n        input: GPIO50",
        "source: internal\n        input: GPIO50\n        output: GPIO23",
        1,
    )
    .replacen("      reset: GPIO51\n", "", 1);
    let board = parse(&yaml).expect("internal clock Board");
    let rust =
        network::network_tokens(board.hardware().ethernet(), None).expect("projected tokens");

    assert!(rust.contains("InternalRefClock<::esp_hal::peripherals::GPIO23<'static>, ::esp_hal::peripherals::GPIO50<'static>>"));
    assert!(rust.contains("InternalRefClock::new(clock_output, clock_input)"));
    assert!(rust.contains("$peripherals.ETH, $peripherals.GPIO50, $peripherals.GPIO23,"));
    assert!(rust.contains("phy_reset: None,"));
}

#[test]
fn a_pin_that_is_not_a_chip_token_is_refused() {
    let yaml = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../boards/configs/m5stack-unit-poep4/board.yml"),
    )
    .expect("repository Board")
    .replacen("mdc: GPIO31", "mdc: GPIO31); evil(", 1);
    let board = parse(&yaml).expect("schema-valid Board");
    assert!(network::network_tokens(board.hardware().ethernet(), None).is_err());
}

#[test]
fn a_board_without_a_link_hands_the_entry_nothing() {
    let board = board("m5stack-stamp-p4");
    assert!(board.hardware().ethernet().is_none());
    assert!(board.hardware().wifi_coprocessor().is_none());
    let rust = network::network_tokens(None, None).expect("projected tokens");
    assert_eq!(rust, network::NO_NETWORK_TOKENS);
    assert!(rust.contains("pub struct NetworkTokens;"));
    assert!(rust.contains("$crate::application::NetworkTokens\n"));
    assert!(rust.contains("macro_rules! __barracuda_esp32p4_network_power"));
}

#[test]
fn a_tab5_hands_the_entry_its_sd_host_co_processor_pins_and_power_switch() {
    for id in ["m5stack-tab5", "m5stack-tab5x"] {
        let board = board(id);
        assert!(board.hardware().ethernet().is_none());
        let rust = network::network_tokens(None, board.hardware().wifi_coprocessor())
            .expect("projected tokens");

        // Slot 1 of the SD host the entry took, then CLK, CMD, D0-D3, and
        // the C6's enable.
        assert!(rust.contains(
            "$crate::application::NetworkTokens::new($sd_host.coprocessor, $peripherals.GPIO12, \
             $peripherals.GPIO13, $peripherals.GPIO11, $peripherals.GPIO10, $peripherals.GPIO9, \
             $peripherals.GPIO8, $peripherals.GPIO15)"
        ));
        assert!(rust.contains("frequency_hz: 40000000,"));
        assert!(rust.contains("chip: 0x0d,"));
        assert!(rust.contains("sdio: crate::sd_host::CoprocessorSlot"));
        assert!(!rust.contains("SDHOST"));
        assert!(rust.contains("($peripherals:ident, $sd_host:ident) => {"));
        assert!(!rust.contains("ethernet:"));
        // WLAN_PWR_EN: output 0 of the PI4IOE5V6408 at 44h on I2C0.
        assert!(rust.contains(
            "$crate::application::power_coprocessor_through_pi4ioe5v6408(\n            \
             $peripherals.I2C0.reborrow(),\n            $peripherals.GPIO32.reborrow(),\n            \
             $peripherals.GPIO31.reborrow(),\n            68,\n            0,\n        );"
        ));
    }
}

#[test]
fn a_board_with_both_links_hands_the_entry_both() {
    let board = board("esp32p4-function-ev-board");
    let rust = network::network_tokens(
        board.hardware().ethernet(),
        board.hardware().wifi_coprocessor(),
    )
    .expect("projected tokens");

    assert!(rust.contains(
        "$peripherals.GPIO52, $peripherals.GPIO51, $sd_host.coprocessor, $peripherals.GPIO18, \
         $peripherals.GPIO19, $peripherals.GPIO14, $peripherals.GPIO15, $peripherals.GPIO16, \
         $peripherals.GPIO17, $peripherals.GPIO54)"
    ));
    assert!(rust
        .contains("pub(crate) ethernet: crate::network::ethernet::EthernetTokens<BoardRmiiPins>,"));
    assert!(
        rust.contains("pub(crate) coprocessor: crate::network::coprocessor::CoprocessorTokens,")
    );
    // The C6 on this Board is always powered.
    assert!(rust.contains("    ($peripherals:ident) => {};"));
}

#[test]
fn co_processor_wiring_the_platform_cannot_drive_is_refused() {
    let tab5 = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards/configs/m5stack-tab5/board.yml"),
    )
    .expect("repository Board");
    let project = |from: &str, to: &str| {
        assert!(tab5.contains(from), "{from}");
        let board = parse(&tab5.replacen(from, to, 1)).expect("schema-valid Board");
        network::network_tokens(None, board.hardware().wifi_coprocessor())
    };
    // Slot 0 is the SD card's fixed IO_MUX slot: Board selection refuses a
    // co-processor there beside the Tab5's card, and the projection refuses
    // it on a Board without one.
    assert!(matches!(
        parse(&tab5.replacen("slot: 1", "slot: 0", 1)),
        Err(barracuda_board_config::ConfigError::SdmmcSlotInUse { slot: 0, .. })
    ));
    let (head, tail) = tab5.split_once("    sdmmc-device:").expect("Tab5 card");
    let (_, rest) = tail.split_once("    i2s-stream:").expect("after the card");
    let without_card = format!("{head}    i2s-stream:{rest}")
        .replacen(
            "    micro-sd:
      implementation: sdmmc-fat-removable-storage
      bindings: {{ device: micro-sd }}
",
            "",
            1,
        )
        .replacen("slot: 1", "slot: 0", 1);
    let cardless = parse(&without_card).expect("Tab5 without its card");
    assert!(network::network_tokens(None, cardless.hardware().wifi_coprocessor()).is_err());
    // ESP-Hosted has no ESP32-H2 co-processor.
    assert!(project("chip: esp32c6", "chip: esp32h2").is_err());
    assert!(project("reset: GPIO15", "reset: GPIO15); evil(").is_err());

    let ev = board("esp32p4-function-ev-board");
    let shared = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../boards/configs/esp32p4-function-ev-board/board.yml"),
    )
    .expect("repository Board")
    .replacen("reset: GPIO54", "reset: GPIO51", 1);
    let shared = parse(&shared).expect("schema-valid Board");
    assert!(network::network_tokens(
        shared.hardware().ethernet(),
        shared.hardware().wifi_coprocessor()
    )
    .is_err());
    assert!(ev.hardware().wifi_coprocessor().is_some());
}
