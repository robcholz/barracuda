//! IP stack devices shared by Platforms: a link that is always down, and one
//! stack over two links.

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

/// Which link a [`FailoverNetworkDriver`] carries traffic on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLink {
    /// No link is up.
    None,
    /// The preferred link.
    Primary,
    /// The fallback link.
    Fallback,
}

/// Network device over a preferred and a fallback link, such as wired
/// Ethernet and a Wi-Fi station on one Board.
///
/// The Platform contract has one IP stack, so a Board with two links runs it
/// over whichever is up, preferring `primary`: frames go out and come in on
/// that link alone, and the stack takes its hardware address. When the active
/// link changes from one live link to the other, [`Driver::link_state`]
/// reports the link down for one poll and wakes the stack, so it restarts
/// DHCP on the new network.
#[derive(Debug)]
pub struct FailoverNetworkDriver<P, F> {
    primary: P,
    fallback: F,
    active: ActiveLink,
}

impl<P: Driver, F: Driver> FailoverNetworkDriver<P, F> {
    /// Combines two links; neither carries traffic until it is up.
    #[must_use]
    pub const fn new(primary: P, fallback: F) -> Self {
        Self {
            primary,
            fallback,
            active: ActiveLink::None,
        }
    }

    /// Returns the link the last link-state poll chose.
    #[must_use]
    pub const fn active(&self) -> ActiveLink {
        self.active
    }
}

/// A token of the link a [`FailoverNetworkDriver`] carries traffic on.
#[derive(Debug)]
pub enum FailoverToken<A, B> {
    /// A token of the preferred link.
    Primary(A),
    /// A token of the fallback link.
    Fallback(B),
}

impl<A: RxToken, B: RxToken> RxToken for FailoverToken<A, B> {
    fn consume<R, G: FnOnce(&mut [u8]) -> R>(self, f: G) -> R {
        match self {
            Self::Primary(token) => token.consume(f),
            Self::Fallback(token) => token.consume(f),
        }
    }
}

impl<A: TxToken, B: TxToken> TxToken for FailoverToken<A, B> {
    fn consume<R, G: FnOnce(&mut [u8]) -> R>(self, len: usize, f: G) -> R {
        match self {
            Self::Primary(token) => token.consume(len, f),
            Self::Fallback(token) => token.consume(len, f),
        }
    }
}

impl<P: Driver, F: Driver> Driver for FailoverNetworkDriver<P, F> {
    type RxToken<'a>
        = FailoverToken<P::RxToken<'a>, F::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = FailoverToken<P::TxToken<'a>, F::TxToken<'a>>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.active {
            ActiveLink::None => None,
            ActiveLink::Primary => self
                .primary
                .receive(cx)
                .map(|(rx, tx)| (FailoverToken::Primary(rx), FailoverToken::Primary(tx))),
            ActiveLink::Fallback => self
                .fallback
                .receive(cx)
                .map(|(rx, tx)| (FailoverToken::Fallback(rx), FailoverToken::Fallback(tx))),
        }
    }

    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        match self.active {
            ActiveLink::None => None,
            ActiveLink::Primary => self.primary.transmit(cx).map(FailoverToken::Primary),
            ActiveLink::Fallback => self.fallback.transmit(cx).map(FailoverToken::Fallback),
        }
    }

    fn link_state(&mut self, cx: &mut Context<'_>) -> LinkState {
        // Both links register the waker, so either changing wakes the stack.
        let primary = self.primary.link_state(cx) == LinkState::Up;
        let fallback = self.fallback.link_state(cx) == LinkState::Up;
        let wanted = match (primary, fallback) {
            (true, _) => ActiveLink::Primary,
            (false, true) => ActiveLink::Fallback,
            (false, false) => ActiveLink::None,
        };
        if wanted != self.active {
            let switching = self.active != ActiveLink::None && wanted != ActiveLink::None;
            self.active = wanted;
            if switching {
                cx.waker().wake_by_ref();
                return LinkState::Down;
            }
        }
        if self.active == ActiveLink::None {
            LinkState::Down
        } else {
            LinkState::Up
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.primary.capabilities();
        capabilities.max_transmission_unit = capabilities
            .max_transmission_unit
            .min(self.fallback.capabilities().max_transmission_unit);
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        match self.active {
            ActiveLink::Fallback => self.fallback.hardware_address(),
            ActiveLink::None | ActiveLink::Primary => self.primary.hardware_address(),
        }
    }
}
