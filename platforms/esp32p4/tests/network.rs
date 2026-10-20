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
        let rust = network::ethernet_tokens(ethernet).expect("projected tokens");

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
    let rust = network::ethernet_tokens(board.hardware().ethernet().expect("Ethernet"))
        .expect("projected tokens");

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
    assert!(network::ethernet_tokens(board.hardware().ethernet().expect("Ethernet")).is_err());
}

#[test]
fn a_board_without_a_link_hands_the_entry_nothing() {
    assert!(board("m5stack-tab5").hardware().ethernet().is_none());
    assert!(network::NO_NETWORK_TOKENS.contains("pub struct NetworkTokens;"));
    assert!(network::NO_NETWORK_TOKENS.contains("$crate::application::NetworkTokens\n"));
}
