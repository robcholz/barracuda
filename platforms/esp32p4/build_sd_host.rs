//! Projects which slots of the ESP32-P4's one SD host the selected Board
//! uses. Shared by `build.rs` and the Platform's host tests.
//!
//! The Platform owns the SD host: it keeps slot 1 for a Wi-Fi co-processor
//! link and hands slot 0, on the fixed IO_MUX pads, to a Board's card.

use std::error::Error;

use barracuda_board_config::BoardDefinition;

/// The SD host's token name on the ESP32-P4.
const SD_HOST: &str = "SDHOST";
/// The slot the Platform hands to a Board's card.
const CARD_SLOT: u8 = 0;

/// Returns whether the selected Board wires a card to slot 0, which the
/// entry then takes for the Board composition.
///
/// # Errors
///
/// Returns an error when a Board card names another controller or slot;
/// Board selection refuses those already, so this guards the build.
pub fn board_card_slot(board: &BoardDefinition) -> Result<bool, Box<dyn Error>> {
    let mut card = false;
    for (name, device) in board.peripheral_io().sdmmc_devices() {
        if device.peripheral() != SD_HOST {
            return Err(format!(
                "SDMMC device `{name}` names `{}`; the ESP32-P4's SD host is `{SD_HOST}`",
                device.peripheral()
            )
            .into());
        }
        if device.slot() != CARD_SLOT {
            return Err(format!(
                "SDMMC device `{name}` is on SD host slot {}; the ESP32-P4 Platform keeps slot 1 for a Wi-Fi co-processor and hands a card slot {CARD_SLOT}",
                device.slot()
            )
            .into());
        }
        card = true;
    }
    Ok(card)
}
