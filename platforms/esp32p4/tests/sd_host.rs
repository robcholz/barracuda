//! Which SD host slots the selected Board's card uses.

#![allow(clippy::expect_used, missing_docs)]

#[path = "../build_sd_host.rs"]
mod sd_host;

use std::{fs, path::Path};

use barracuda_board_config::parse;

fn board_yaml(id: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../boards/configs")
            .join(id)
            .join("board.yml"),
    )
    .expect("repository Board")
}

#[test]
fn boards_with_a_card_hand_slot_0_to_the_board_composition() {
    for id in ["m5stack-tab5", "m5stack-tab5x", "esp32p4-function-ev-board"] {
        let board = parse(&board_yaml(id)).expect("valid Board");
        assert_eq!(sd_host::board_card_slot(&board).ok(), Some(true), "{id}");
        // The co-processor keeps slot 1 beside it.
        let coprocessor = board.hardware().wifi_coprocessor().expect("co-processor");
        assert_eq!(coprocessor.sdio().slot(), 1, "{id}");
        let card = board
            .peripheral_io()
            .sdmmc_device("micro-sd")
            .expect("microSD slot");
        assert_eq!(
            (card.bus_width(), card.data().len(), card.frequency_hz()),
            (4, 4, 40_000_000),
            "{id}"
        );
        assert_eq!(
            (card.power_channel(), card.power_millivolts()),
            (Some(4), Some(3300)),
            "{id}"
        );
    }
    for id in ["m5stack-unit-poep4", "m5stack-stamp-p4"] {
        let board = parse(&board_yaml(id)).expect("valid Board");
        assert_eq!(sd_host::board_card_slot(&board).ok(), Some(false), "{id}");
    }
}

#[test]
fn a_card_off_slot_0_or_off_the_sd_host_breaks_the_build() {
    // Without its co-processor the Board could name slot 1; the Platform
    // still hands a card slot 0 only.
    let yaml = board_yaml("m5stack-stamp-p4");
    let card = "peripherals:\n  io:\n    sdmmc-device:\n      micro-sd:\n        peripheral: SDHOST\n        slot: 1\n        clk: GPIO43\n        cmd: GPIO44\n        data: [GPIO39]\n        bus-width: 1\n";
    let with_card = if yaml.contains("peripherals:\n  io:\n") {
        yaml.replacen("peripherals:\n  io:\n", card, 1)
    } else {
        yaml.replacen("native-layout:", &format!("{card}native-layout:"), 1)
    };
    let board = parse(&with_card).expect("schema-valid Board");
    assert!(sd_host::board_card_slot(&board).is_err());

    let other_host = with_card.replacen(
        "peripheral: SDHOST\n        slot: 1",
        "peripheral: SDMMC1\n        slot: 0",
        1,
    );
    let board = parse(&other_host).expect("schema-valid Board");
    assert!(sd_host::board_card_slot(&board).is_err());
}
