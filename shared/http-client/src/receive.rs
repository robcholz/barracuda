//! Dedicated connections for long-lived receive loops.
//!
//! Model calls and ordinary sends share the request pool behind
//! [`ClientFactory::create`]. A channel that holds a long poll or a push
//! connection open must never take one of those connections, so receive loops
//! lease a slot from [`ReceiveSlots`] instead. Each slot owns one TCP socket
//! with its own static buffers and shares the System's DNS resolver and TLS
//! engine; a lease holds at most one open connection at a time.
//!
//! The slots' socket buffers are static: the application declares one
//! [`ReceiveBuffers`] sized at build time from the selected Platform's
//! long-lived connection budget, so a Target without receive connections
//! links no receive buffers at all. [`ReceiveSlots::capacity`] is the runtime
//! limit System derives from the selected Board's memory; it never exceeds
//! the static size.

#[cfg(feature = "mbedtls")]
use alloc::ffi::CString;
use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::{Cell, RefCell, RefMut},
    fmt,
    future::poll_fn,
    net::{IpAddr, SocketAddr},
    task::{Poll, Waker},
};

#[cfg(feature = "mbedtls")]
use embassy_net::tcp::{TcpReader, TcpWriter};
use embassy_net::{
    tcp::{Error as TcpError, TcpSocket},
    IpAddress, Stack,
};
use embassy_time::Duration;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use static_cell::ConstStaticCell;

use crate::{ClientFactory, Resolver};

/// Bytes a receive connection buffers for sending. Requests, heartbeats and
/// TLS handshake records are small; larger writes wait for acknowledgements.
const RECEIVE_TX_BYTES: usize = 1024;
/// Bytes a receive connection buffers as it arrives, which is also the TCP
/// window it advertises.
const RECEIVE_RX_BYTES: usize = 4 * 1024;

/// Idle time after which a receive connection probes its peer.
const KEEP_ALIVE: Duration = Duration::from_secs(30);
/// Time without an answer (to a connection attempt, data, or a keep-alive
/// probe) after which a receive connection is dropped as dead.
const DEAD_PEER_TIMEOUT: Duration = Duration::from_secs(60);

/// Send and receive buffers of one receive slot: 1 KiB and 4 KiB.
pub struct ReceiveSlotBuffers {
    tx: [u8; RECEIVE_TX_BYTES],
    rx: [u8; RECEIVE_RX_BYTES],
}

impl ReceiveSlotBuffers {
    /// Zeroed buffers, for static storage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tx: [0; RECEIVE_TX_BYTES],
            rx: [0; RECEIVE_RX_BYTES],
        }
    }
}

impl Default for ReceiveSlotBuffers {
    fn default() -> Self {
        Self::new()
    }
}

/// Static socket buffers for `N` receive slots.
///
/// The application declares exactly one, sized from the selected Platform's
/// long-lived connection budget, so the buffers are placed in `.bss` at link
/// time and a Target that allows none links no buffers:
///
/// ```rust,ignore
/// static RECEIVE_BUFFERS: ReceiveBuffers<{ barracuda_target::RECEIVE_SLOTS }> =
///     ReceiveBuffers::new();
/// ```
pub struct ReceiveBuffers<const N: usize> {
    buffers: ConstStaticCell<[ReceiveSlotBuffers; N]>,
}

impl<const N: usize> ReceiveBuffers<N> {
    /// Zeroed buffers for `N` slots.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffers: ConstStaticCell::new([const { ReceiveSlotBuffers::new() }; N]),
        }
    }

    /// Hands out the buffers once; later calls get none.
    pub fn take(&'static self) -> &'static mut [ReceiveSlotBuffers] {
        match self.buffers.try_take() {
            Some(buffers) => buffers.as_mut_slice(),
            None => &mut [],
        }
    }
}

impl<const N: usize> Default for ReceiveBuffers<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The TCP socket of one receive slot.
///
/// It connects one remote endpoint at a time: a second connection attempt
/// while a [`ReceiveConnection`] is open fails.
pub struct ReceiveSocket {
    socket: RefCell<TcpSocket<'static>>,
}

impl ReceiveSocket {
    fn new(stack: Stack<'static>, buffers: &'static mut ReceiveSlotBuffers) -> Self {
        let ReceiveSlotBuffers { tx, rx } = buffers;
        let mut socket = TcpSocket::new(stack, rx, tx);
        socket.set_keep_alive(Some(KEEP_ALIVE));
        socket.set_timeout(Some(DEAD_PEER_TIMEOUT));
        Self {
            socket: RefCell::new(socket),
        }
    }

    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "the borrow is the connection's exclusive hold on the socket; \
                  every other user tries to borrow and fails as busy"
    )]
    async fn open(&self, remote: SocketAddr) -> Result<ReceiveConnection<'_>, StreamError> {
        let mut socket = self
            .socket
            .try_borrow_mut()
            .map_err(|_| StreamError::Busy)?;
        let address = match remote.ip() {
            IpAddr::V4(address) => IpAddress::Ipv4(address),
            IpAddr::V6(_) => return Err(StreamError::Connect),
        };
        // A connection that ended without a clean close may leave the socket
        // open; a new connection always starts from a closed socket.
        socket.abort();
        socket
            .connect((address, remote.port()))
            .await
            .map_err(|_| StreamError::Connect)?;
        Ok(ReceiveConnection { socket })
    }
}

impl TcpConnect for ReceiveSocket {
    type Error = TcpError;
    type Connection<'a>
        = ReceiveConnection<'a>
    where
        Self: 'a;

    async fn connect<'a>(&'a self, remote: SocketAddr) -> Result<ReceiveConnection<'a>, TcpError> {
        self.open(remote)
            .await
            .map_err(|_| TcpError::ConnectionReset)
    }
}

/// One open connection of a receive slot. Dropping it resets the
/// connection and frees the slot's socket for the next one.
pub struct ReceiveConnection<'a> {
    socket: RefMut<'a, TcpSocket<'static>>,
}

impl Drop for ReceiveConnection<'_> {
    fn drop(&mut self) {
        self.socket.abort();
    }
}

impl ErrorType for ReceiveConnection<'_> {
    type Error = TcpError;
}

impl Read for ReceiveConnection<'_> {
    // cancel-safe: embassy-net keeps unread bytes in the socket buffer.
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TcpError> {
        self.socket.read(buffer).await
    }
}

impl Write for ReceiveConnection<'_> {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, TcpError> {
        self.socket.write(bytes).await
    }

    async fn flush(&mut self) -> Result<(), TcpError> {
        self.socket.flush().await
    }
}

#[cfg(feature = "mbedtls")]
impl barracuda_tls::Split for ReceiveConnection<'_> {
    type Read<'b>
        = TcpReader<'b>
    where
        Self: 'b;
    type Write<'b>
        = TcpWriter<'b>
    where
        Self: 'b;

    fn split(&mut self) -> (TcpReader<'_>, TcpWriter<'_>) {
        self.socket.split()
    }
}

/// One receive slot: its connector and the shared DNS and TLS it uses.
struct Slot<C, D: 'static> {
    connector: C,
    resolver: &'static D,
    #[cfg(feature = "mbedtls")]
    tls: Option<Rc<barracuda_tls::Tls>>,
    leased: Cell<bool>,
}

struct Pool<C, D: 'static> {
    slots: Vec<Rc<Slot<C, D>>>,
    /// Tasks waiting in [`ReceiveSlots::acquire_when_free`].
    waiting: RefCell<Vec<Waker>>,
}

/// The receive-connection slots System sizes for the selected Target.
///
/// Plugins receive it in `PluginContext::receive_slots` and clone it freely;
/// every clone shares the same slots. A channel takes a [`ReceiveLease`] while
/// it receives and drops it when it stops.
pub struct ReceiveSlots<C = ReceiveSocket, D: 'static = Resolver> {
    pool: Rc<Pool<C, D>>,
}

impl<C, D> Clone for ReceiveSlots<C, D> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<C, D> fmt::Debug for ReceiveSlots<C, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceiveSlots")
            .field("capacity", &self.capacity())
            .field("in_use", &self.in_use())
            .finish()
    }
}

impl<C, D> Default for ReceiveSlots<C, D> {
    fn default() -> Self {
        Self::unavailable()
    }
}

impl ReceiveSlots {
    /// Creates up to `limit` receive slots on `stack`, one per entry of
    /// `buffers`, sharing the DNS resolver and TLS engine of `shared`.
    ///
    /// Each slot registers one socket with the stack for the life of the
    /// process, so the stack's socket table must have room for them.
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        shared: &ClientFactory<'static>,
        buffers: &'static mut [ReceiveSlotBuffers],
        limit: usize,
    ) -> Self {
        let slots = buffers
            .iter_mut()
            .take(limit)
            .map(|buffers| {
                Rc::new(Slot {
                    connector: ReceiveSocket::new(stack, buffers),
                    resolver: shared.resolver,
                    #[cfg(feature = "mbedtls")]
                    tls: shared.tls.clone(),
                    leased: Cell::new(false),
                })
            })
            .collect();
        Self::from_slots(slots)
    }
}

impl<C, D> ReceiveSlots<C, D> {
    /// Slots for a Target without receive connections: nothing can be
    /// acquired and the capacity is zero.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::from_slots(Vec::new())
    }

    /// Creates one plaintext slot per connector, for a network that is not
    /// the System's (tests, or a subsystem that owns its own stack).
    #[must_use]
    pub fn from_connectors(connectors: impl IntoIterator<Item = C>, resolver: &'static D) -> Self {
        let slots = connectors
            .into_iter()
            .map(|connector| {
                Rc::new(Slot {
                    connector,
                    resolver,
                    #[cfg(feature = "mbedtls")]
                    tls: None,
                    leased: Cell::new(false),
                })
            })
            .collect();
        Self::from_slots(slots)
    }

    fn from_slots(slots: Vec<Rc<Slot<C, D>>>) -> Self {
        Self {
            pool: Rc::new(Pool {
                slots,
                waiting: RefCell::new(Vec::new()),
            }),
        }
    }

    /// How many receive connections this Target may hold at once.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pool.slots.len()
    }

    /// How many slots are leased now.
    #[must_use]
    pub fn in_use(&self) -> usize {
        self.pool
            .slots
            .iter()
            .filter(|slot| slot.leased.get())
            .count()
    }

    /// Leases a free slot, or returns `None` when every slot is in use.
    #[must_use]
    pub fn acquire(&self) -> Option<ReceiveLease<C, D>> {
        let slot = self.pool.slots.iter().find(|slot| !slot.leased.get())?;
        slot.leased.set(true);
        Some(ReceiveLease {
            slot: slot.clone(),
            pool: self.pool.clone(),
        })
    }

    /// Waits until a slot is free, without leasing it.
    ///
    /// Completes immediately when a slot is free now. A dropped
    /// [`ReceiveLease`] wakes every waiter, so another task may still take the
    /// freed slot first: follow it with [`ReceiveSlots::acquire`] and wait
    /// again when that returns `None`, or use
    /// [`ReceiveSlots::acquire_when_free`]. Never completes when the capacity
    /// is zero.
    ///
    /// # Cancel safety
    ///
    /// Cancel-safe: it changes no slot.
    pub async fn wait_available(&self) {
        poll_fn(|context| {
            if self.in_use() < self.capacity() {
                return Poll::Ready(());
            }
            self.wait_for_release(context.waker());
            Poll::Pending
        })
        .await;
    }

    /// Waits until a slot is free and leases it.
    ///
    /// Never completes when the capacity is zero.
    ///
    /// # Cancel safety
    ///
    /// Cancel-safe: a slot is leased only in the poll that returns it.
    pub async fn acquire_when_free(&self) -> ReceiveLease<C, D> {
        poll_fn(|context| {
            if let Some(lease) = self.acquire() {
                return Poll::Ready(lease);
            }
            self.wait_for_release(context.waker());
            Poll::Pending
        })
        .await
    }

    /// Registers `waker` to be woken when a lease is dropped.
    fn wait_for_release(&self, waker: &Waker) {
        let mut waiting = self.pool.waiting.borrow_mut();
        if !waiting.iter().any(|waiting| waiting.will_wake(waker)) {
            waiting.push(waker.clone());
        }
    }
}

/// One leased receive slot. Dropping it frees the slot and wakes the tasks
/// waiting for one.
pub struct ReceiveLease<C = ReceiveSocket, D: 'static = Resolver> {
    slot: Rc<Slot<C, D>>,
    pool: Rc<Pool<C, D>>,
}

impl<C, D> fmt::Debug for ReceiveLease<C, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceiveLease")
            .finish_non_exhaustive()
    }
}

impl<C, D> Drop for ReceiveLease<C, D> {
    fn drop(&mut self) {
        self.slot.leased.set(false);
        let waiting = core::mem::take(&mut *self.pool.waiting.borrow_mut());
        for waker in waiting {
            waker.wake();
        }
    }
}

impl<C, D> ReceiveLease<C, D>
where
    C: TcpConnect,
    D: Dns,
{
    /// HTTP clients over this slot's single connection, with the System's
    /// DNS resolver and TLS engine.
    ///
    /// Every client created from it shares the slot's one connection: a
    /// request on a second client while the first holds its connection fails
    /// to connect.
    #[must_use]
    pub fn client_factory(&self) -> ClientFactory<'_, C, D> {
        ClientFactory {
            tcp: &self.slot.connector,
            resolver: self.slot.resolver,
            #[cfg(feature = "mbedtls")]
            tls: self.slot.tls.clone(),
        }
    }
}

impl<D: Dns> ReceiveLease<ReceiveSocket, D> {
    /// Opens a raw byte stream to the host and port of `url`: TLS for
    /// `https://` and `wss://`, plain TCP for `http://` and `ws://`. The path
    /// is ignored; a protocol such as WebSocket writes its own request.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError`] when the URL is unsupported, the lease already
    /// holds a connection, or resolving, connecting, or the TLS handshake
    /// fails.
    ///
    /// # Cancel safety
    ///
    /// Dropping the future before it completes resets the half-open
    /// connection and leaves the lease reusable.
    pub async fn connect_stream(&self, url: &str) -> Result<ReceiveStream<'_>, StreamError> {
        let endpoint = Endpoint::parse(url)?;
        let address = self
            .slot
            .resolver
            .get_host_by_name(endpoint.host, AddrType::Either)
            .await
            .map_err(|_| StreamError::Dns)?;
        let connection = self
            .slot
            .connector
            .open(SocketAddr::new(address, endpoint.port))
            .await?;
        if !endpoint.tls {
            return Ok(ReceiveStream {
                link: Link::Plain(connection),
            });
        }
        #[cfg(feature = "mbedtls")]
        {
            let tls = self.slot.tls.as_ref().ok_or(StreamError::TlsUnavailable)?;
            let server_name = CString::new(endpoint.host).map_err(|_| StreamError::InvalidUrl)?;
            let mut session = tls.client_session(connection, &server_name)?;
            session.connect().await?;
            Ok(ReceiveStream {
                link: Link::Tls(session),
            })
        }
        #[cfg(not(feature = "mbedtls"))]
        {
            drop(connection);
            Err(StreamError::TlsUnavailable)
        }
    }
}

/// A raw byte stream opened by [`ReceiveLease::connect_stream`].
///
/// Reading and writing the whole stream is not cancel-safe for TLS. A
/// protocol that waits for incoming data while it also sends (heartbeats, for
/// example) uses [`ReceiveStream::split`], whose read half is cancel-safe.
pub struct ReceiveStream<'a> {
    link: Link<'a>,
}

enum Link<'a> {
    Plain(ReceiveConnection<'a>),
    #[cfg(feature = "mbedtls")]
    Tls(barracuda_tls::Session<'static, ReceiveConnection<'a>>),
}

impl<'a> ReceiveStream<'a> {
    /// Whether the stream is encrypted.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        match &self.link {
            Link::Plain(_) => false,
            #[cfg(feature = "mbedtls")]
            Link::Tls(_) => true,
        }
    }

    /// Splits the stream into a read half and a write half that may be used
    /// concurrently.
    ///
    /// The read half's `read` is cancel-safe: a dropped read loses no data, so
    /// it can be raced against a timer or a write. Writes are not cancel-safe
    /// and are awaited to completion.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError`] if a TLS stream fails to finish its handshake;
    /// a stream from [`ReceiveLease::connect_stream`] has already finished it.
    pub async fn split<'s>(
        &'s mut self,
    ) -> Result<
        (
            impl Read<Error = StreamError> + use<'a, 's>,
            impl Write<Error = StreamError> + use<'a, 's>,
        ),
        StreamError,
    > {
        match &mut self.link {
            Link::Plain(connection) => {
                let (reader, writer) = connection.socket.split();
                #[cfg(feature = "mbedtls")]
                return Ok((Half::Plain(reader), Half::Plain(writer)));
                #[cfg(not(feature = "mbedtls"))]
                return Ok((
                    Half::<_, NoTls>::Plain(reader),
                    Half::<_, NoTls>::Plain(writer),
                ));
            }
            #[cfg(feature = "mbedtls")]
            Link::Tls(session) => {
                let (reader, writer) = session.split().await?;
                Ok((Half::Tls(reader), Half::Tls(writer)))
            }
        }
    }
}

impl ErrorType for ReceiveStream<'_> {
    type Error = StreamError;
}

impl Read for ReceiveStream<'_> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, StreamError> {
        match &mut self.link {
            Link::Plain(connection) => Ok(connection.read(buffer).await?),
            #[cfg(feature = "mbedtls")]
            Link::Tls(session) => Ok(session.read(buffer).await?),
        }
    }
}

impl Write for ReceiveStream<'_> {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, StreamError> {
        match &mut self.link {
            Link::Plain(connection) => Ok(connection.write(bytes).await?),
            #[cfg(feature = "mbedtls")]
            Link::Tls(session) => Ok(session.write(bytes).await?),
        }
    }

    async fn flush(&mut self) -> Result<(), StreamError> {
        match &mut self.link {
            Link::Plain(connection) => Ok(connection.flush().await?),
            #[cfg(feature = "mbedtls")]
            Link::Tls(session) => Ok(session.flush().await?),
        }
    }
}

/// One half of a split [`ReceiveStream`].
enum Half<P, T> {
    Plain(P),
    #[cfg_attr(
        not(feature = "mbedtls"),
        allow(dead_code, reason = "a build without TLS has no TLS half")
    )]
    Tls(T),
}

impl<P, T> ErrorType for Half<P, T> {
    type Error = StreamError;
}

impl<P, T> Read for Half<P, T>
where
    P: Read,
    T: Read,
    StreamError: From<P::Error> + From<T::Error>,
{
    // cancel-safe: both a TCP read half and a TLS read half are.
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, StreamError> {
        match self {
            Self::Plain(half) => Ok(half.read(buffer).await?),
            Self::Tls(half) => Ok(half.read(buffer).await?),
        }
    }
}

impl<P, T> Write for Half<P, T>
where
    P: Write,
    T: Write,
    StreamError: From<P::Error> + From<T::Error>,
{
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, StreamError> {
        match self {
            Self::Plain(half) => Ok(half.write(bytes).await?),
            Self::Tls(half) => Ok(half.write(bytes).await?),
        }
    }

    async fn flush(&mut self) -> Result<(), StreamError> {
        match self {
            Self::Plain(half) => Ok(half.flush().await?),
            Self::Tls(half) => Ok(half.flush().await?),
        }
    }
}

/// The TLS half of a stream in a build without TLS: it cannot exist.
#[cfg(not(feature = "mbedtls"))]
enum NoTls {}

#[cfg(not(feature = "mbedtls"))]
impl ErrorType for NoTls {
    type Error = StreamError;
}

#[cfg(not(feature = "mbedtls"))]
impl Read for NoTls {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, StreamError> {
        match *self {}
    }
}

#[cfg(not(feature = "mbedtls"))]
impl Write for NoTls {
    async fn write(&mut self, _bytes: &[u8]) -> Result<usize, StreamError> {
        match *self {}
    }

    async fn flush(&mut self) -> Result<(), StreamError> {
        match *self {}
    }
}

/// Failure to open or use a [`ReceiveStream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamError {
    /// The URL is not an `http`, `https`, `ws`, or `wss` URL with a host.
    InvalidUrl,
    /// The lease already holds an open connection.
    Busy,
    /// The host name did not resolve.
    Dns,
    /// The TCP connection could not be established.
    Connect,
    /// A TLS URL was given but the System has no TLS engine.
    TlsUnavailable,
    /// The TLS handshake or a TLS record failed with this mbedTLS error code.
    Tls(i32),
    /// The connection failed while reading or writing.
    Io(ErrorKind),
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("unsupported stream URL"),
            Self::Busy => formatter.write_str("the receive slot already has a connection"),
            Self::Dns => formatter.write_str("the host name did not resolve"),
            Self::Connect => formatter.write_str("the connection could not be established"),
            Self::TlsUnavailable => formatter.write_str("TLS is unavailable"),
            Self::Tls(code) => write!(
                formatter,
                "TLS failed with mbedTLS error -0x{:04x}",
                code.unsigned_abs()
            ),
            Self::Io(kind) => write!(formatter, "connection failed: {kind:?}"),
        }
    }
}

impl core::error::Error for StreamError {}

impl embedded_io_async::Error for StreamError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidUrl => ErrorKind::InvalidInput,
            Self::Busy => ErrorKind::AddrInUse,
            Self::Dns | Self::Connect => ErrorKind::NotConnected,
            Self::TlsUnavailable => ErrorKind::Unsupported,
            Self::Tls(_) => ErrorKind::Other,
            Self::Io(kind) => *kind,
        }
    }
}

impl From<TcpError> for StreamError {
    fn from(error: TcpError) -> Self {
        match error {
            TcpError::ConnectionReset => Self::Io(ErrorKind::ConnectionReset),
        }
    }
}

#[cfg(feature = "mbedtls")]
impl From<barracuda_tls::SessionError> for StreamError {
    fn from(error: barracuda_tls::SessionError) -> Self {
        match error {
            barracuda_tls::SessionError::MbedTls(error) => Self::Tls(error.code()),
            barracuda_tls::SessionError::Io(kind) => Self::Io(kind),
        }
    }
}

/// The host and port a stream URL names.
#[derive(Debug, PartialEq, Eq)]
struct Endpoint<'a> {
    host: &'a str,
    port: u16,
    tls: bool,
}

impl<'a> Endpoint<'a> {
    fn parse(url: &'a str) -> Result<Self, StreamError> {
        let (scheme, rest) = url.split_once("://").ok_or(StreamError::InvalidUrl)?;
        let (tls, default_port) =
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("wss") {
                (true, 443)
            } else if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("ws") {
                (false, 80)
            } else {
                return Err(StreamError::InvalidUrl);
            };
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .ok_or(StreamError::InvalidUrl)?;
        // User information and IPv6 literals are not supported.
        if authority.contains(['@', '[', ']']) {
            return Err(StreamError::InvalidUrl);
        }
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>().map_err(|_| StreamError::InvalidUrl)?,
            ),
            None => (authority, default_port),
        };
        if host.is_empty() || port == 0 {
            return Err(StreamError::InvalidUrl);
        }
        Ok(Self { host, port, tls })
    }
}

#[cfg(test)]
mod tests {
    use super::{Endpoint, ReceiveBuffers, ReceiveSlotBuffers, StreamError};

    #[test]
    fn static_buffers_scale_with_the_slot_count() {
        let slot = core::mem::size_of::<ReceiveSlotBuffers>();
        assert_eq!(slot, 5 * 1024);
        // A Target without receive connections keeps only the taken flag.
        assert!(core::mem::size_of::<ReceiveBuffers<0>>() <= core::mem::align_of::<usize>());
        let three = core::mem::size_of::<ReceiveBuffers<3>>();
        assert!(three >= 3 * slot && three <= 3 * slot + core::mem::align_of::<usize>());
    }

    #[test]
    fn stream_urls_name_a_host_port_and_security() {
        assert_eq!(
            Endpoint::parse("wss://api.sgroup.qq.com/websocket"),
            Ok(Endpoint {
                host: "api.sgroup.qq.com",
                port: 443,
                tls: true
            })
        );
        assert_eq!(
            Endpoint::parse("HTTP://10.42.0.1:18787/health?x=1"),
            Ok(Endpoint {
                host: "10.42.0.1",
                port: 18787,
                tls: false
            })
        );
        assert_eq!(
            Endpoint::parse("ws://gateway.local#fragment"),
            Ok(Endpoint {
                host: "gateway.local",
                port: 80,
                tls: false
            })
        );
        for invalid in [
            "ftp://example.com",
            "example.com",
            "https://",
            "https://:443",
            "https://example.com:0",
            "https://example.com:99999",
            "https://user@example.com",
            "https://[::1]:443",
        ] {
            assert_eq!(
                Endpoint::parse(invalid),
                Err(StreamError::InvalidUrl),
                "{invalid}"
            );
        }
    }
}
