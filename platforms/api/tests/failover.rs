//! One IP stack over a preferred and a fallback link.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::task::{Context, Waker};

use barracuda_platform::{ActiveLink, FailoverNetworkDriver};
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

/// The frames a link sent.
type Sent = Rc<RefCell<Vec<Vec<u8>>>>;

/// A link whose state the test switches, with a MAC of six copies of `mac`,
/// that hands out one-byte frames carrying `mac` and records what it sends.
struct Link {
    mac: u8,
    mtu: usize,
    up: Rc<Cell<bool>>,
    sent: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl Link {
    fn new(mac: u8, mtu: usize) -> (Self, Rc<Cell<bool>>, Sent) {
        let up = Rc::new(Cell::new(false));
        let sent = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                mac,
                mtu,
                up: Rc::clone(&up),
                sent: Rc::clone(&sent),
            },
            up,
            sent,
        )
    }
}

struct Frame(u8);

impl RxToken for Frame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, f: F) -> R {
        f(&mut [self.0])
    }
}

struct Send(Rc<RefCell<Vec<Vec<u8>>>>);

impl TxToken for Send {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let result = f(&mut frame);
        self.0.borrow_mut().push(frame);
        result
    }
}

impl Driver for Link {
    type RxToken<'a> = Frame;
    type TxToken<'a> = Send;

    fn receive(&mut self, _cx: &mut Context<'_>) -> Option<(Frame, Send)> {
        Some((Frame(self.mac), Send(Rc::clone(&self.sent))))
    }

    fn transmit(&mut self, _cx: &mut Context<'_>) -> Option<Send> {
        Some(Send(Rc::clone(&self.sent)))
    }

    fn link_state(&mut self, _cx: &mut Context<'_>) -> LinkState {
        if self.up.get() {
            LinkState::Up
        } else {
            LinkState::Down
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = self.mtu;
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet([self.mac; 6])
    }
}

type Pair = FailoverNetworkDriver<Link, Link>;

fn poll(driver: &mut Pair) -> LinkState {
    driver.link_state(&mut Context::from_waker(Waker::noop()))
}

fn received(driver: &mut Pair) -> Option<u8> {
    let (rx, _) = driver.receive(&mut Context::from_waker(Waker::noop()))?;
    Some(rx.consume(|frame| frame[0]))
}

fn send(driver: &mut Pair, byte: u8) -> bool {
    driver
        .transmit(&mut Context::from_waker(Waker::noop()))
        .map(|token| token.consume(1, |frame| frame[0] = byte))
        .is_some()
}

#[test]
fn nothing_flows_until_a_link_is_up() {
    let (primary, _, _) = Link::new(1, 1514);
    let (fallback, _, _) = Link::new(2, 1514);
    let mut driver = Pair::new(primary, fallback);
    assert!(poll(&mut driver) == LinkState::Down);
    assert_eq!(driver.active(), ActiveLink::None);
    assert_eq!(received(&mut driver), None);
    assert!(!send(&mut driver, 0));
    assert_eq!(driver.hardware_address(), HardwareAddress::Ethernet([1; 6]));
}

#[test]
fn the_fallback_carries_the_stack_while_the_preferred_link_is_down() {
    let (primary, _, primary_sent) = Link::new(1, 1514);
    let (fallback, fallback_up, fallback_sent) = Link::new(2, 1500);
    let mut driver = Pair::new(primary, fallback);
    fallback_up.set(true);

    assert!(poll(&mut driver) == LinkState::Up);
    assert_eq!(driver.active(), ActiveLink::Fallback);
    assert_eq!(received(&mut driver), Some(2));
    assert!(send(&mut driver, 7));
    assert_eq!(*fallback_sent.borrow(), [vec![7]]);
    assert!(primary_sent.borrow().is_empty());
    assert_eq!(driver.hardware_address(), HardwareAddress::Ethernet([2; 6]));
    assert_eq!(driver.capabilities().max_transmission_unit, 1500);
}

#[test]
fn moving_between_live_links_drops_the_link_for_one_poll() {
    let (primary, primary_up, primary_sent) = Link::new(1, 1514);
    let (fallback, fallback_up, _) = Link::new(2, 1514);
    let mut driver = Pair::new(primary, fallback);
    fallback_up.set(true);
    assert!(poll(&mut driver) == LinkState::Up);
    assert_eq!(driver.active(), ActiveLink::Fallback);

    // The cable comes up: one poll reports the link down so the stack
    // restarts DHCP, then the preferred link carries it.
    primary_up.set(true);
    assert!(poll(&mut driver) == LinkState::Down);
    assert_eq!(driver.active(), ActiveLink::Primary);
    assert!(poll(&mut driver) == LinkState::Up);
    assert_eq!(received(&mut driver), Some(1));
    assert!(send(&mut driver, 3));
    assert_eq!(*primary_sent.borrow(), [vec![3]]);
    assert_eq!(driver.hardware_address(), HardwareAddress::Ethernet([1; 6]));

    // The cable goes: back to Wi-Fi through one more drop.
    primary_up.set(false);
    assert!(poll(&mut driver) == LinkState::Down);
    assert!(poll(&mut driver) == LinkState::Up);
    assert_eq!(driver.active(), ActiveLink::Fallback);

    // Both down: down, with no drop to report.
    fallback_up.set(false);
    assert!(poll(&mut driver) == LinkState::Down);
    assert_eq!(driver.active(), ActiveLink::None);
}
