//! IP stack device for Platforms without a network driver yet.

use core::task::Context;

use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

/// Network device whose link is always down.
///
/// It lets a Platform without a network driver, such as ESP32-P4 without a
/// radio or STM32 before its Ethernet is wired, still provide the IP stack
/// the Platform contract requires. The stack never carries traffic; pair it
/// with [`crate::UnavailableWifiDevice`].
#[derive(Debug, Default)]
pub struct UnavailableNetworkDriver;

/// Token for a link that never sends or receives.
#[derive(Debug)]
pub enum UnavailableToken {}

impl RxToken for UnavailableToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _f: F) -> R {
        match self {}
    }
}

impl TxToken for UnavailableToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _len: usize, _f: F) -> R {
        match self {}
    }
}

impl Driver for UnavailableNetworkDriver {
    type RxToken<'a> = UnavailableToken;
    type TxToken<'a> = UnavailableToken;

    fn receive(&mut self, _cx: &mut Context<'_>) -> Option<(UnavailableToken, UnavailableToken)> {
        None
    }

    fn transmit(&mut self, _cx: &mut Context<'_>) -> Option<UnavailableToken> {
        None
    }

    fn link_state(&mut self, _cx: &mut Context<'_>) -> LinkState {
        LinkState::Down
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = 1500;
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        // An IP-native link needs no hardware address; nothing is ever sent.
        HardwareAddress::Ip
    }
}
