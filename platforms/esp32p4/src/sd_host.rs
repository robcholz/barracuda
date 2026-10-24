//! The ESP32-P4's one SD host, which the Platform owns.
//!
//! The SD host has two slots that share one transfer engine. A Board wires a
//! Wi-Fi co-processor's SDIO link to slot 1, which reaches any pin through
//! the GPIO matrix, and a microSD card to slot 0, on the fixed IO_MUX pads
//! GPIO39-44. Both need the one `SDHOST` token, so neither the co-processor
//! link nor the Board composition builds the controller: the Platform entry
//! builds it once, before any task runs, and takes the slots the selected
//! Board uses (`build.rs` projects them into `barracuda_esp32p4_*` cfgs). It
//! keeps slot 1 for its co-processor link and hands slot 0 to the Board
//! composition, whose `sdmmc-device` binding takes it from [`BoardSdHost`].
//!
//! Both slots run interrupt-driven ([`Async`]): esp-hal serializes them on
//! the shared engine with an async mutex, first come first served, so the
//! card waits for a co-processor transfer instead of failing as busy, and
//! the reverse. The card keeps each of its transfers short (`sd_card`).

use esp_hal::peripherals::SDHOST;
#[cfg(any(barracuda_esp32p4_wifi_coprocessor, barracuda_esp32p4_sd_card))]
use esp_hal::sdmmc::{Config, ConfigError, SdHostController, Slot, SlotConfig};
#[cfg(barracuda_esp32p4_wifi_coprocessor)]
use esp_hal::Async;

/// Slot 1, for the Board's Wi-Fi co-processor link, or why the SD host
/// refused it.
#[cfg(barracuda_esp32p4_wifi_coprocessor)]
pub type CoprocessorSlot = Result<Slot<'static, 1, Async>, ConfigError>;

/// The SD host's slots the selected Board uses, taken once by the entry.
pub struct SdHost {
    /// Slot 1, which the Platform keeps for its co-processor link; the
    /// entry moves it into the network tokens.
    #[cfg(barracuda_esp32p4_wifi_coprocessor)]
    pub coprocessor: CoprocessorSlot,
    /// The slots the entry hands the Board composition.
    pub board: BoardSdHost,
}

/// The SD host slots the Board composition takes through its
/// `sdmmc-device` bindings.
pub struct BoardSdHost {
    /// Slot 0, for the Board's card.
    #[cfg(barracuda_esp32p4_sd_card)]
    pub slot0: crate::hal::SdCardSlot,
}

impl SdHost {
    /// Builds the SD host from its token, when the selected Board uses it,
    /// and takes the slots the Board is wired to.
    ///
    /// A controller or slot the SD host refuses is reported by whoever uses
    /// that slot: the co-processor link when the Platform starts, the card
    /// when the Board composition builds it.
    #[must_use]
    pub fn take(sdhost: SDHOST<'static>) -> Self {
        #[cfg(any(barracuda_esp32p4_wifi_coprocessor, barracuda_esp32p4_sd_card))]
        let controller: Result<&'static SdHostController<'static>, ConfigError> = {
            static CONTROLLER: static_cell::StaticCell<SdHostController<'static>> =
                static_cell::StaticCell::new();
            // Starting the SD host also ties LDO VO4, which powers slot 0's
            // pads, to the 3.3 V rail.
            SdHostController::new(sdhost, Config::default())
                .map(|controller| &*CONTROLLER.init(controller))
        };
        #[cfg(not(any(barracuda_esp32p4_wifi_coprocessor, barracuda_esp32p4_sd_card)))]
        let _unused = sdhost;
        Self {
            #[cfg(barracuda_esp32p4_wifi_coprocessor)]
            coprocessor: controller
                .and_then(|controller| controller.slot::<1>(SlotConfig::default()))
                .map(Slot::into_async),
            board: BoardSdHost {
                #[cfg(barracuda_esp32p4_sd_card)]
                slot0: crate::hal::SdCardSlot::new(
                    controller
                        .and_then(|controller| controller.slot::<0>(SlotConfig::default()))
                        .map(Slot::into_async),
                ),
            },
        }
    }
}
