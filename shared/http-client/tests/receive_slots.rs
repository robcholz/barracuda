//! Receive slots: capacity, leasing, and the wake-up when a lease drops.
#![allow(
    clippy::disallowed_types,
    reason = "host-only test wakers use std atomics and Arc"
)]
#![allow(clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use embedded_io_async::ErrorKind;
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use http_client::ReceiveSlots;

/// A network on which nothing connects.
struct Offline;

impl TcpConnect for Offline {
    type Error = ErrorKind;
    type Connection<'a> = Closed;

    async fn connect(&self, _remote: SocketAddr) -> Result<Closed, ErrorKind> {
        Err(ErrorKind::NotConnected)
    }
}

impl Dns for Offline {
    type Error = ErrorKind;

    async fn get_host_by_name(&self, _host: &str, _kind: AddrType) -> Result<IpAddr, ErrorKind> {
        Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    async fn get_host_by_address(
        &self,
        _address: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }
}

struct Closed;

impl embedded_io_async::ErrorType for Closed {
    type Error = ErrorKind;
}

impl embedded_io_async::Read for Closed {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, ErrorKind> {
        Err(ErrorKind::NotConnected)
    }
}

impl embedded_io_async::Write for Closed {
    async fn write(&mut self, _bytes: &[u8]) -> Result<usize, ErrorKind> {
        Err(ErrorKind::NotConnected)
    }

    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Err(ErrorKind::NotConnected)
    }
}

static RESOLVER: Offline = Offline;

fn slots(count: usize) -> ReceiveSlots<Offline, Offline> {
    ReceiveSlots::from_connectors((0..count).map(|_| Offline), &RESOLVER)
}

#[derive(Default)]
struct CountingWaker(AtomicUsize);

impl Wake for CountingWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll_once<F: Future>(future: std::pin::Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(waker))
}

#[test]
fn leases_are_limited_to_the_capacity_and_freed_on_drop() {
    let slots = slots(2);
    assert_eq!((slots.capacity(), slots.in_use()), (2, 0));

    let first = slots.acquire().expect("first slot");
    let shared = slots.clone();
    let second = shared.acquire().expect("second slot");
    assert_eq!(slots.in_use(), 2);
    assert!(slots.acquire().is_none(), "no third slot");

    drop(first);
    assert_eq!(slots.in_use(), 1);
    let third = slots.acquire().expect("the freed slot");
    assert_eq!(shared.in_use(), 2);
    drop((second, third));
    assert_eq!(slots.in_use(), 0);
}

#[test]
fn an_unavailable_pool_has_no_slots() {
    let slots = ReceiveSlots::<Offline, Offline>::unavailable();
    assert_eq!((slots.capacity(), slots.in_use()), (0, 0));
    assert!(slots.acquire().is_none());
    assert_eq!(
        format!("{slots:?}"),
        "ReceiveSlots { capacity: 0, in_use: 0 }"
    );
}

#[test]
fn a_dropped_lease_wakes_tasks_waiting_for_a_slot() {
    let slots = slots(1);
    let held = slots.acquire().expect("the only slot");
    let counter = Arc::new(CountingWaker::default());
    let waker = Waker::from(counter.clone());

    let mut available = pin!(slots.wait_available());
    let mut leased = pin!(slots.acquire_when_free());
    assert!(poll_once(available.as_mut(), &waker).is_pending());
    assert!(poll_once(leased.as_mut(), &waker).is_pending());
    assert_eq!(counter.0.load(Ordering::SeqCst), 0);

    drop(held);
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "the waiter was woken"
    );
    assert!(poll_once(available.as_mut(), &waker).is_ready());
    let Poll::Ready(lease) = poll_once(leased.as_mut(), &waker) else {
        panic!("a free slot is leased");
    };
    assert_eq!(slots.in_use(), 1);
    drop(lease);
}

#[test]
fn a_lease_creates_clients_over_its_own_connector() {
    let slots = slots(1);
    let lease = slots.acquire().expect("slot");
    let factory = lease.client_factory();
    let (_client, tls) = factory.create();
    assert!(!tls, "slots from connectors are plaintext");
}
