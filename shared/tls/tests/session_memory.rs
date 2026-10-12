//! Heap held by one client TLS session, measured against an in-process server.
//!
//! A client and a server session handshake over an in-memory pipe. Every
//! allocation is tagged with the side that made it, so the figures below are
//! the client's alone, split the way devices split them: the ordinary heap
//! (internal RAM) and bulk memory (PSRAM where a Board has it), which mbedTLS
//! uses for its allocations of 4 KiB or more.
//!
//! Run with `--nocapture` to print the figures. The assertions are budgets for
//! the record-buffer sizes `shared/tls` compiles mbedTLS with.
#![allow(
    unsafe_code,
    clippy::disallowed_types,
    clippy::expect_used,
    clippy::panic,
    reason = "a host-only measurement harness with its own counting allocator"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::CStr;
use std::future::Future;
use std::pin::{pin, Pin};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use allocator_api2::alloc::{AllocError, Allocator};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_tls::Tls;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use mbedtls_rs::sys::{
    psa_crypto_init, MBEDTLS_SSL_IN_CONTENT_LEN, MBEDTLS_SSL_OUT_CONTENT_LEN,
    MBEDTLS_X509_BADCERT_NOT_TRUSTED,
};
use mbedtls_rs::{
    AuthMode, Certificate, ClientSessionConfig, Credentials, PrivateKey, ServerSessionConfig,
    Session, SessionConfig, TlsVersion, X509,
};

/// The server's leaf and intermediate, as a server sends them. RSA-2048
/// throughout, like the chains of the messaging services Barracuda polls.
const SERVER_CHAIN: &str = concat!(include_str!("fixtures/test-server-chain.pem"), "\0");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/test-server-key.der");

/// A request and response shaped like one long-poll round trip.
const REQUEST: &[u8] = b"POST /bot/getUpdates HTTP/1.1\r\nHost: localhost\r\n\
Content-Type: application/json\r\nContent-Length: 46\r\n\r\n\
{\"offset\":1,\"timeout\":50,\"allowed_updates\":[]}";
const RESPONSE_BYTES: usize = 1200;

/// One record's worth of application data each way.
const LARGE_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// Allocation accounting

const OTHER: usize = 0;
const CLIENT: usize = 1;
const SERVER: usize = 2;
const ROLES: usize = 3;

thread_local! {
    static ROLE: Cell<usize> = const { Cell::new(OTHER) };
}

/// Current and peak bytes per role in one memory domain.
struct Ledger {
    current: [AtomicUsize; ROLES],
    peak: [AtomicUsize; ROLES],
}

impl Ledger {
    const fn new() -> Self {
        Self {
            current: [const { AtomicUsize::new(0) }; ROLES],
            peak: [const { AtomicUsize::new(0) }; ROLES],
        }
    }

    fn add(&self, role: usize, bytes: usize) {
        let now = self.current[role].fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.peak[role].fetch_max(now, Ordering::Relaxed);
    }

    fn sub(&self, role: usize, bytes: usize) {
        self.current[role].fetch_sub(bytes, Ordering::Relaxed);
    }

    fn current(&self, role: usize) -> usize {
        self.current[role].load(Ordering::Relaxed)
    }

    fn peak(&self, role: usize) -> usize {
        self.peak[role].load(Ordering::Relaxed)
    }

    fn reset_peak(&self, role: usize) {
        self.peak[role].store(self.current(role), Ordering::Relaxed);
    }
}

static ORDINARY: Ledger = Ledger::new();
static BULK: Ledger = Ledger::new();

/// Bytes in front of every allocation: the role and the requested size.
const TAG: usize = 16;

/// Allocates `layout` from the system with a role tag in front of it.
///
/// # Safety
///
/// `layout` has a non-zero size.
unsafe fn tagged_alloc(ledger: &Ledger, layout: Layout) -> *mut u8 {
    let align = layout.align().max(TAG);
    let Ok(outer) = Layout::from_size_align(layout.size() + align, align) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `outer` has a non-zero size.
    let base = unsafe { System.alloc(outer) };
    if base.is_null() {
        return base;
    }
    let role = ROLE.with(Cell::get);
    // SAFETY: the tag fits in the `align >= TAG` bytes before the result.
    unsafe {
        let user = base.add(align);
        user.sub(TAG).cast::<usize>().write(role);
        user.sub(TAG / 2).cast::<usize>().write(layout.size());
        ledger.add(role, layout.size());
        user
    }
}

/// Frees an allocation made by [`tagged_alloc`] with the same `layout`.
///
/// # Safety
///
/// `user` came from [`tagged_alloc`] with `layout` and is freed once.
unsafe fn tagged_free(ledger: &Ledger, user: *mut u8, layout: Layout) {
    let align = layout.align().max(TAG);
    // SAFETY: the caller passes a live tagged allocation.
    unsafe {
        let role = user.sub(TAG).cast::<usize>().read();
        let size = user.sub(TAG / 2).cast::<usize>().read();
        ledger.sub(role, size);
        let outer = Layout::from_size_align_unchecked(layout.size() + align, align);
        System.dealloc(user.sub(align), outer);
    }
}

struct Ordinary;

// SAFETY: forwards to the system allocator with a fixed-size prefix.
unsafe impl GlobalAlloc for Ordinary {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `GlobalAlloc` callers never pass a zero size.
        unsafe { tagged_alloc(&ORDINARY, layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from `alloc` with `layout`.
        unsafe { tagged_free(&ORDINARY, pointer, layout) }
    }
}

#[global_allocator]
static GLOBAL: Ordinary = Ordinary;

struct Bulk;

// SAFETY: forwards to the system allocator with a fixed-size prefix.
unsafe impl Allocator for Bulk {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Err(AllocError);
        }
        // SAFETY: the size is non-zero.
        let pointer = unsafe { tagged_alloc(&BULK, layout) };
        NonNull::new(pointer)
            .map(|pointer| NonNull::slice_from_raw_parts(pointer, layout.size()))
            .ok_or(AllocError)
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        // SAFETY: `pointer` came from `allocate` with `layout`.
        unsafe { tagged_free(&BULK, pointer.as_ptr(), layout) }
    }
}

static BULK_ALLOCATOR: Bulk = Bulk;
static BULK_BACKEND: barracuda_bulk_memory::platform::Backend =
    barracuda_bulk_memory::platform::Backend::new(&BULK_ALLOCATOR);

/// Runs `poll` with allocations attributed to `role`.
fn as_role<T>(role: usize, poll: impl FnOnce() -> T) -> T {
    let previous = ROLE.with(|current| current.replace(role));
    let result = poll();
    ROLE.with(|current| current.set(previous));
    result
}

/// A future whose polls allocate as `role`.
struct Acting<F> {
    role: usize,
    future: Pin<Box<F>>,
}

impl<F: Future> Future for Acting<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<F::Output> {
        let role = self.role;
        as_role(role, || self.future.as_mut().poll(context))
    }
}

fn acting<F: Future>(role: usize, future: F) -> Acting<F> {
    Acting {
        role,
        future: as_role(OTHER, || Box::pin(future)),
    }
}

/// Polls both futures to completion on this thread.
fn run_both<A: Future, B: Future>(first: A, second: B) -> (A::Output, B::Output) {
    let mut first = pin!(first);
    let mut second = pin!(second);
    let mut first_output = None;
    let mut second_output = None;
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..1_000_000 {
        if first_output.is_none() {
            if let Poll::Ready(output) = first.as_mut().poll(&mut context) {
                first_output = Some(output);
            }
        }
        if second_output.is_none() {
            if let Poll::Ready(output) = second.as_mut().poll(&mut context) {
                second_output = Some(output);
            }
        }
        if let (Some(_), Some(_)) = (&first_output, &second_output) {
            return (
                first_output.expect("first output"),
                second_output.expect("second output"),
            );
        }
    }
    panic!("the two sides stopped making progress");
}

// ---------------------------------------------------------------------------
// An in-memory duplex stream

struct Pipe {
    bytes: VecDeque<u8>,
}

#[derive(Clone)]
struct End {
    incoming: Rc<RefCell<Pipe>>,
    outgoing: Rc<RefCell<Pipe>>,
}

/// Two connected ends whose buffers are reserved up front, so they never
/// allocate while a session is measured.
fn pipe() -> (End, End) {
    let new = || {
        Rc::new(RefCell::new(Pipe {
            bytes: VecDeque::with_capacity(4 * LARGE_BYTES),
        }))
    };
    let (forward, backward) = (new(), new());
    (
        End {
            incoming: backward.clone(),
            outgoing: forward.clone(),
        },
        End {
            incoming: forward,
            outgoing: backward,
        },
    )
}

impl ErrorType for End {
    type Error = ErrorKind;
}

impl Read for End {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, ErrorKind> {
        std::future::poll_fn(|_context| {
            let mut pipe = self.incoming.borrow_mut();
            if pipe.bytes.is_empty() {
                return Poll::Pending;
            }
            let count = buffer.len().min(pipe.bytes.len());
            for (slot, byte) in buffer.iter_mut().zip(pipe.bytes.drain(..count)) {
                *slot = byte;
            }
            Poll::Ready(Ok(count))
        })
        .await
    }
}

impl Write for End {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, ErrorKind> {
        let mut pipe = self.outgoing.borrow_mut();
        assert!(
            pipe.bytes.len() + bytes.len() <= pipe.bytes.capacity(),
            "the pipe would grow while measured"
        );
        pipe.bytes.extend(bytes);
        Ok(bytes.len())
    }

    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sessions

#[derive(Clone)]
struct FixedEntropy;

impl Entropy for FixedEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = index.to_le_bytes()[0] ^ 0xa5;
        }
        Ok(())
    }
}

/// Trusts the top of the test chain, standing in for the bundled roots.
unsafe extern "C" fn trust_test_chain(
    _context: *mut core::ffi::c_void,
    _certificate: *mut mbedtls_rs::sys::mbedtls_x509_crt,
    _depth: core::ffi::c_int,
    flags: *mut u32,
) -> core::ffi::c_int {
    // SAFETY: mbedTLS passes a valid flags pointer.
    unsafe { *flags &= !MBEDTLS_X509_BADCERT_NOT_TRUSTED };
    0
}

struct Fixture {
    tls: Tls,
    roots: Certificate<'static>,
    server: Credentials<'static>,
}

impl Fixture {
    fn new() -> Self {
        barracuda_bulk_memory::platform::install(&BULK_BACKEND);
        let tls = Tls::new(FixedEntropy).expect("start the TLS engine");
        // SAFETY: the engine's random source is installed.
        assert_eq!(unsafe { psa_crypto_init() }, 0);
        let chain = CStr::from_bytes_with_nul(SERVER_CHAIN.as_bytes()).expect("PEM chain");
        Self {
            tls,
            roots: Certificate::verified_by(trust_test_chain).expect("verify callback"),
            server: Credentials {
                certificate: Certificate::new(X509::PEM(chain)).expect("server chain"),
                private_key: PrivateKey::new(X509::DER(SERVER_KEY), None).expect("server key"),
            },
        }
    }

    /// A client configured as `shared/http-client` configures every client.
    fn client(&self, stream: End) -> Session<'static, End> {
        let config = SessionConfig::Client(ClientSessionConfig {
            ca_chain: Some(self.roots.clone()),
            creds: None,
            server_name: None,
            auth_mode: AuthMode::Required,
            min_version: self.tls.version(),
            alpn_protocols: None,
        });
        let mut session =
            Session::new(self.tls.reference(), stream, &config).expect("client session");
        session
            .set_server_name(c"localhost")
            .expect("client server name");
        session
    }

    fn server(&self, stream: End) -> Session<'static, End> {
        let config = SessionConfig::Server(ServerSessionConfig {
            ca_chain: None,
            creds: self.server.clone(),
            auth_mode: AuthMode::None,
            min_version: TlsVersion::Tls1_2,
            alpn_protocols: None,
        });
        Session::new(self.tls.reference(), stream, &config).expect("server session")
    }
}

/// Reads exactly `bytes.len()` bytes.
async fn read_exact(session: &mut Session<'static, End>, bytes: &mut [u8]) {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = session.read(&mut bytes[filled..]).await.expect("read");
        assert_ne!(count, 0, "the peer closed early");
        filled += count;
    }
}

async fn write_all(session: &mut Session<'static, End>, bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        written += session.write(&bytes[written..]).await.expect("write");
    }
    session.flush().await.expect("flush");
}

/// One request and response over an established pair.
fn round_trip(client: &mut Session<'static, End>, server: &mut Session<'static, End>) {
    let response = vec![b'r'; RESPONSE_BYTES];
    let mut client_received = vec![0; RESPONSE_BYTES];
    let mut server_received = vec![0; REQUEST.len()];
    run_both(
        acting(CLIENT, async {
            write_all(client, REQUEST).await;
            read_exact(client, &mut client_received).await;
        }),
        acting(SERVER, async {
            read_exact(server, &mut server_received).await;
            write_all(server, &response).await;
        }),
    );
    assert_eq!(client_received, response);
    assert_eq!(server_received, REQUEST);
}

struct Figures {
    setup: (usize, usize),
    handshake_peak: (usize, usize),
    idle: (usize, usize),
    transfer_peak: (usize, usize),
}

fn client_now() -> (usize, usize) {
    (ORDINARY.current(CLIENT), BULK.current(CLIENT))
}

fn client_peak() -> (usize, usize) {
    (ORDINARY.peak(CLIENT), BULK.peak(CLIENT))
}

fn measure(fixture: &Fixture) -> Figures {
    let (client_end, server_end) = as_role(OTHER, pipe);
    let mut client = as_role(CLIENT, || fixture.client(client_end));
    let mut server = as_role(SERVER, || fixture.server(server_end));
    let setup = client_now();

    ORDINARY.reset_peak(CLIENT);
    BULK.reset_peak(CLIENT);
    let (connected, accepted) = run_both(
        acting(CLIENT, client.connect()),
        acting(SERVER, server.connect()),
    );
    connected.expect("client handshake");
    accepted.expect("server handshake");
    let handshake_peak = client_peak();

    round_trip(&mut client, &mut server);
    let idle = client_now();

    // A full 16 KiB record arrives in one piece, and a 16 KiB write leaves in
    // records no larger than the outgoing buffer.
    ORDINARY.reset_peak(CLIENT);
    BULK.reset_peak(CLIENT);
    let large: Vec<u8> = (0..LARGE_BYTES)
        .map(|index| index.to_le_bytes()[0])
        .collect();
    let mut client_received = vec![0; LARGE_BYTES];
    let mut server_received = vec![0; LARGE_BYTES];
    run_both(
        acting(CLIENT, async {
            read_exact(&mut client, &mut client_received).await;
            write_all(&mut client, &large).await;
        }),
        acting(SERVER, async {
            write_all(&mut server, &large).await;
            read_exact(&mut server, &mut server_received).await;
        }),
    );
    assert_eq!(client_received, large);
    assert_eq!(server_received, large);
    let transfer_peak = client_peak();
    assert_eq!(
        client_now(),
        idle,
        "a transfer leaves the idle session as it was"
    );

    drop(client);
    drop(server);
    assert_eq!(
        client_now(),
        (0, 0),
        "a dropped client session frees everything"
    );
    Figures {
        setup,
        handshake_peak,
        idle,
        transfer_peak,
    }
}

#[test]
fn one_idle_client_session_fits_its_budget() {
    let fixture = as_role(OTHER, Fixture::new);
    // The first handshake initializes process-wide state, such as PSA's key
    // slots, that later sessions reuse.
    let _warm_up = measure(&fixture);
    let figures = measure(&fixture);

    println!(
        "mbedTLS record content: in {MBEDTLS_SSL_IN_CONTENT_LEN} B, out {MBEDTLS_SSL_OUT_CONTENT_LEN} B"
    );
    println!("client session bytes        ordinary     bulk    total");
    for (label, (ordinary, bulk)) in [
        ("after Session::new", figures.setup),
        ("handshake peak", figures.handshake_peak),
        ("idle after a round trip", figures.idle),
        ("16 KiB each way, peak", figures.transfer_peak),
    ] {
        println!(
            "  {label:<26} {ordinary:>8} {bulk:>8} {:>8}",
            ordinary + bulk
        );
    }

    // Inbound records stay 16 KiB capable: servers send full-size records.
    assert_eq!(MBEDTLS_SSL_IN_CONTENT_LEN, 16384);
    let out_content = MBEDTLS_SSL_OUT_CONTENT_LEN as usize;
    let (idle_ordinary, idle_bulk) = figures.idle;
    let (peak_ordinary, peak_bulk) = figures.handshake_peak;
    // Small structures, the peer's certificate chain among them, stay on the
    // ordinary heap.
    assert!(
        idle_ordinary <= 16 * 1024,
        "idle ordinary heap {idle_ordinary}"
    );
    assert!(
        peak_ordinary <= 20 * 1024,
        "handshake ordinary peak {peak_ordinary}"
    );
    // Bulk memory holds the two record buffers, and during the handshake one
    // more buffer the size of the outgoing one.
    assert!(
        idle_bulk <= 16_800 + out_content + 600,
        "idle bulk {idle_bulk}"
    );
    assert!(
        peak_bulk <= idle_bulk + out_content + 512,
        "handshake bulk peak {peak_bulk}"
    );
}
