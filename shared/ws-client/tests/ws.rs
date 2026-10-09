//! The WebSocket client over in-memory pipes, with the RFC 6455 examples.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    missing_docs
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use futures_lite::future::block_on;
use ws_client::{
    accept_key, connect, encode_key, CloseFrame, HandshakeError, Limits, Message, Request, Url,
    WsError, WsReader, WsWriter,
};

/// RFC 6455 section 5.7 mask.
const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

#[derive(Default)]
struct PipeState {
    bytes: VecDeque<u8>,
    closed: bool,
    waker: Option<Waker>,
}

/// Server-to-client bytes; reads return at most `max_read` bytes and wait
/// while the pipe is empty.
#[derive(Clone, Default)]
struct Incoming {
    state: Rc<RefCell<PipeState>>,
    max_read: usize,
}

impl Incoming {
    fn new(max_read: usize) -> Self {
        Self {
            state: Rc::default(),
            max_read,
        }
    }

    fn push(&self, bytes: &[u8]) {
        let mut state = self.state.borrow_mut();
        state.bytes.extend(bytes);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

#[derive(Debug)]
struct PipeError;

impl std::fmt::Display for PipeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("pipe failed")
    }
}

impl std::error::Error for PipeError {}

impl embedded_io_async::Error for PipeError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::BrokenPipe
    }
}

impl ErrorType for Incoming {
    type Error = PipeError;
}

impl Read for Incoming {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, PipeError> {
        std::future::poll_fn(|context| {
            let mut state = self.state.borrow_mut();
            if state.bytes.is_empty() {
                if state.closed {
                    return Poll::Ready(Ok(0));
                }
                state.waker = Some(context.waker().clone());
                return Poll::Pending;
            }
            let count = buffer.len().min(state.bytes.len()).min(self.max_read);
            for slot in buffer.iter_mut().take(count) {
                *slot = state.bytes.pop_front().unwrap();
            }
            Poll::Ready(Ok(count))
        })
        .await
    }
}

/// Client-to-server bytes, recorded.
#[derive(Clone, Default)]
struct Outgoing(Rc<RefCell<Vec<u8>>>);

impl Outgoing {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl ErrorType for Outgoing {
    type Error = PipeError;
}

impl Write for Outgoing {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, PipeError> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    async fn flush(&mut self) -> Result<(), PipeError> {
        Ok(())
    }
}

type Mask = fn() -> [u8; 4];

fn fixed_mask() -> [u8; 4] {
    MASK
}

fn open_reader(bytes: &[u8], max_read: usize, limits: Limits) -> (WsReader<Incoming>, Incoming) {
    let incoming = Incoming::new(max_read);
    incoming.push(bytes);
    (WsReader::new(incoming.clone(), limits), incoming)
}

fn writer() -> (WsWriter<Outgoing, Mask>, Outgoing) {
    let outgoing = Outgoing::default();
    (
        WsWriter::new(outgoing.clone(), fixed_mask as Mask),
        outgoing,
    )
}

fn text(message: Result<Message<'_>, WsError>) -> String {
    match message.expect("a message") {
        Message::Text(text) => text.to_owned(),
        other => panic!("expected text, got {other:?}"),
    }
}

/// Polls `future` once; `None` while it is pending.
fn poll_once<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

// --- Handshake -----------------------------------------------------------

#[test]
fn accept_key_matches_rfc_6455_example() {
    assert_eq!(
        &accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
        b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
    // The RFC's key is the base64 of the bytes "the sample nonce".
    assert_eq!(
        &encode_key(b"the sample nonce"),
        b"dGhlIHNhbXBsZSBub25jZQ=="
    );
}

#[test]
fn urls_name_host_path_and_tls() {
    assert_eq!(
        Url::parse("wss://api.sgroup.qq.com/websocket/"),
        Ok(Url {
            host: "api.sgroup.qq.com",
            path: "/websocket/",
            tls: true
        })
    );
    assert_eq!(
        Url::parse("WS://10.0.0.2:8080"),
        Ok(Url {
            host: "10.0.0.2:8080",
            path: "/",
            tls: false
        })
    );
    for invalid in [
        "https://x/",
        "wss://",
        "wss://user@host/",
        "x",
        "wss://:443/",
    ] {
        assert_eq!(Url::parse(invalid), Err(WsError::InvalidUrl), "{invalid}");
    }
}

fn upgrade_response(accept: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )
}

#[test]
fn connect_writes_the_upgrade_and_keeps_bytes_after_the_headers() {
    let incoming = Incoming::new(7);
    let mut response = upgrade_response("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=").into_bytes();
    // The server's first frame arrives in the same segment as its headers.
    response.extend_from_slice(&[0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
    incoming.push(&response);
    let outgoing = Outgoing::default();
    let url = Url::parse("wss://gateway.test/websocket").unwrap();
    let request = Request {
        headers: &[("User-Agent", "barracuda")],
        ..Request::new(url, *b"the sample nonce")
    };
    let (mut reader, _writer) = block_on(connect(
        incoming,
        outgoing.clone(),
        &request,
        fixed_mask as Mask,
        Limits::DEFAULT,
    ))
    .expect("upgraded");
    let written = String::from_utf8(outgoing.take()).unwrap();
    assert_eq!(
        written,
        "GET /websocket HTTP/1.1\r\nHost: gateway.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nUser-Agent: barracuda\r\n\r\n"
    );
    assert_eq!(text(block_on(reader.next())), "Hello");
}

fn handshake_error(response: &str) -> WsError {
    let incoming = Incoming::new(usize::MAX);
    incoming.push(response.as_bytes());
    incoming.close();
    let url = Url::parse("ws://gateway.test/").unwrap();
    match block_on(connect(
        incoming,
        Outgoing::default(),
        &Request::new(url, *b"the sample nonce"),
        fixed_mask as Mask,
        Limits::DEFAULT,
    )) {
        Ok(_) => panic!("handshake accepted: {response}"),
        Err(error) => error,
    }
}

#[test]
fn connect_rejects_a_server_that_does_not_upgrade() {
    assert_eq!(
        handshake_error("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n"),
        WsError::Handshake(HandshakeError::Status(403))
    );
    assert_eq!(
        handshake_error(&upgrade_response("wrongwrongwrongwrongwrong=")),
        WsError::Handshake(HandshakeError::BadAccept)
    );
    assert_eq!(
        handshake_error(
            "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n"
        ),
        WsError::Handshake(HandshakeError::NotUpgraded)
    );
    assert_eq!(
        handshake_error("SSH-2.0-OpenSSH\r\n\r\n"),
        WsError::Handshake(HandshakeError::Malformed)
    );
    assert_eq!(handshake_error("HTTP/1.1 101 Swi"), WsError::Eof);
    let long = format!("HTTP/1.1 101 OK\r\nX: {}\r\n\r\n", "a".repeat(4096));
    assert_eq!(
        handshake_error(&long),
        WsError::Handshake(HandshakeError::HeadersTooLarge)
    );
}

// --- RFC 6455 section 5.7 examples ----------------------------------------

#[test]
fn reads_the_rfc_unmasked_text_and_fragmented_text() {
    let (mut reader, _incoming) = open_reader(
        &[
            0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f, // "Hello"
            0x01, 0x03, 0x48, 0x65, 0x6c, // "Hel"
            0x80, 0x02, 0x6c, 0x6f, // "lo"
        ],
        usize::MAX,
        Limits::DEFAULT,
    );
    assert_eq!(text(block_on(reader.next())), "Hello");
    assert_eq!(text(block_on(reader.next())), "Hello");
}

#[test]
fn reads_the_rfc_ping_and_answers_with_the_rfc_masked_pong() {
    let (mut reader, _incoming) = open_reader(
        &[0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f],
        usize::MAX,
        Limits::DEFAULT,
    );
    let Message::Ping(payload) = block_on(reader.next()).unwrap() else {
        panic!("expected a ping");
    };
    let (mut writer, outgoing) = writer();
    block_on(writer.pong(payload)).unwrap();
    assert_eq!(
        outgoing.take(),
        [0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]
    );
}

#[test]
fn writes_the_rfc_masked_text() {
    let (mut writer, outgoing) = writer();
    block_on(writer.send_text("Hello")).unwrap();
    assert_eq!(
        outgoing.take(),
        [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]
    );
}

#[test]
fn reads_the_rfc_256_byte_and_64_kib_binary_messages() {
    let mut bytes = vec![0x82, 0x7E, 0x01, 0x00];
    bytes.extend((0..256).map(|index| index as u8));
    bytes.extend([0x82, 0x7F, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00]);
    bytes.extend(std::iter::repeat_n(0xA5, 65536));
    let (mut reader, _incoming) = open_reader(&bytes, 1000, Limits::DEFAULT);
    match block_on(reader.next()).unwrap() {
        Message::Binary(payload) => {
            assert_eq!(payload.len(), 256);
            assert_eq!(payload[255], 255);
        }
        other => panic!("{other:?}"),
    }
    match block_on(reader.next()).unwrap() {
        Message::Binary(payload) => {
            assert_eq!(payload.len(), 65536);
            assert!(payload.iter().all(|byte| *byte == 0xA5));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn writes_extended_lengths() {
    let (mut writer, outgoing) = writer();
    block_on(writer.send_binary(&[0; 256])).unwrap();
    let frame = outgoing.take();
    assert_eq!(
        &frame[..8],
        &[0x82, 0xFE, 0x01, 0x00, 0x37, 0xfa, 0x21, 0x3d]
    );
    assert_eq!(frame.len(), 8 + 256);
    assert_eq!(&frame[8..12], &MASK);
    block_on(writer.send_binary(&[0; 65536])).unwrap();
    let frame = outgoing.take();
    assert_eq!(
        &frame[..14],
        &[0x82, 0xFF, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00, 0x37, 0xfa, 0x21, 0x3d]
    );
    assert_eq!(frame.len(), 14 + 65536);
}

// --- Limits, fragmentation, and control frames ----------------------------

#[test]
fn a_frame_or_message_over_the_limit_is_too_large() {
    let limits = Limits {
        max_message: 8,
        ..Limits::DEFAULT
    };
    let (mut reader, _incoming) = open_reader(&[0x81, 0x09, b'x'], usize::MAX, limits);
    assert_eq!(block_on(reader.next()), Err(WsError::TooLarge));
    let (mut reader, _incoming) = open_reader(
        &[
            0x01, 0x05, b'a', b'b', b'c', b'd', b'e', 0x80, 0x04, b'f', b'g', b'h', b'i',
        ],
        usize::MAX,
        limits,
    );
    assert_eq!(block_on(reader.next()), Err(WsError::TooLarge));
    // The 64 KiB default rejects one byte more than 64 KiB before reading it.
    let (mut reader, _incoming) = open_reader(
        &[0x82, 0x7F, 0, 0, 0, 0, 0, 0x01, 0x00, 0x01],
        usize::MAX,
        Limits::DEFAULT,
    );
    assert_eq!(block_on(reader.next()), Err(WsError::TooLarge));
}

#[test]
fn control_frames_interleave_with_fragments() {
    let (mut reader, _incoming) = open_reader(
        &[
            0x01, 0x03, b'H', b'e', b'l', // first fragment
            0x89, 0x01, b'p', // ping in between
            0x00, 0x01, b'l', // continuation
            0x8A, 0x00, // pong
            0x80, 0x01, b'o', // final fragment
        ],
        1,
        Limits::DEFAULT,
    );
    assert_eq!(block_on(reader.next()), Ok(Message::Ping(b"p")));
    assert_eq!(block_on(reader.next()), Ok(Message::Pong(b"")));
    assert_eq!(text(block_on(reader.next())), "Hello");
}

#[test]
fn close_frames_carry_code_and_reason() {
    let (mut reader, _incoming) = open_reader(
        &[0x88, 0x06, 0x0F, 0xA9, b'b', b'y', b'e', b'!', 0x88, 0x00],
        usize::MAX,
        Limits::DEFAULT,
    );
    assert_eq!(
        block_on(reader.next()),
        Ok(Message::Close(Some(CloseFrame {
            code: 4009,
            reason: "bye!"
        })))
    );
    assert_eq!(block_on(reader.next()), Ok(Message::Close(None)));
    let (mut writer, outgoing) = writer();
    block_on(writer.close(1000, "")).unwrap();
    let frame = outgoing.take();
    assert_eq!(&frame[..2], &[0x88, 0x82]);
    assert_eq!(frame[6] ^ MASK[0], 0x03);
    assert_eq!(frame[7] ^ MASK[1], 0xE8);
    block_on(writer.close(1000, &"é".repeat(100))).unwrap();
    assert_eq!(outgoing.take()[1], 0x80 | 124);
}

#[test]
fn framing_violations_are_protocol_errors() {
    for (bytes, expected) in [
        (
            &[0x81, 0x85, 0, 0, 0, 0, b'x'][..],
            WsError::Protocol("server frames must not be masked"),
        ),
        (
            &[0xC1, 0x00][..],
            WsError::Protocol("reserved bits set without an extension"),
        ),
        (
            &[0x80, 0x00][..],
            WsError::Protocol("continuation without a message"),
        ),
        (
            &[0x09, 0x00][..],
            WsError::Protocol("fragmented control frame"),
        ),
        (&[0x89, 0x7E, 0x00, 0x7E][..], WsError::ControlTooLong),
        (&[0x83, 0x00][..], WsError::Protocol("unknown opcode")),
        (
            &[0x81, 0x7E, 0x00, 0x05][..],
            WsError::Protocol("length not minimally encoded"),
        ),
        (
            &[0x01, 0x01, b'a', 0x81, 0x01, b'b'][..],
            WsError::Protocol("new message inside a fragmented one"),
        ),
        (&[0x81, 0x02, 0xC3, 0x28][..], WsError::InvalidUtf8),
        (
            &[0x88, 0x01, 0x03][..],
            WsError::Protocol("close payload of one byte"),
        ),
    ] {
        let (mut reader, _incoming) = open_reader(bytes, usize::MAX, Limits::DEFAULT);
        assert_eq!(block_on(reader.next()), Err(expected), "{bytes:02x?}");
    }
    let (mut writer, _outgoing) = writer();
    assert_eq!(
        block_on(writer.ping(&[0; 126])),
        Err(WsError::ControlTooLong)
    );
}

#[test]
fn an_ended_stream_is_eof() {
    let (mut reader, incoming) = open_reader(&[0x81, 0x05, b'H'], usize::MAX, Limits::DEFAULT);
    incoming.close();
    assert_eq!(block_on(reader.next()), Err(WsError::Eof));
}

// --- Cancel safety and memory ---------------------------------------------

#[test]
fn a_cancelled_read_loses_no_bytes() {
    let (mut reader, incoming) = open_reader(&[0x81, 0x05, b'H', b'e'], 1, Limits::DEFAULT);
    // Each poll takes one byte, then waits; dropping the future between
    // polls (as a heartbeat timer winning a race does) keeps them.
    for _ in 0..6 {
        assert!(poll_once(reader.next()).is_none());
    }
    incoming.push(b"llo");
    assert_eq!(text(block_on(reader.next())), "Hello");
    // The same holds in the middle of a fragmented message.
    incoming.push(&[0x01, 0x02, b'a', b'b', 0x80]);
    for _ in 0..3 {
        assert!(poll_once(reader.next()).is_none());
    }
    incoming.push(&[0x01, b'c']);
    assert_eq!(text(block_on(reader.next())), "abc");
}

#[test]
fn the_buffer_grows_for_a_large_frame_and_shrinks_back() {
    let limits = Limits {
        idle_buffer: 64,
        ..Limits::DEFAULT
    };
    let mut bytes = vec![0x82, 0x7E, 0x10, 0x00];
    bytes.extend(std::iter::repeat_n(1, 4096));
    bytes.extend([0x81, 0x02, b'o', b'k']);
    let (mut reader, _incoming) = open_reader(&bytes, 512, limits);
    assert!(reader.buffered_capacity() <= 64);
    match block_on(reader.next()).unwrap() {
        Message::Binary(payload) => assert_eq!(payload.len(), 4096),
        other => panic!("{other:?}"),
    }
    assert!(reader.buffered_capacity() >= 4096);
    assert_eq!(text(block_on(reader.next())), "ok");
    // Once the buffer drains, it returns to its idle size.
    let pending = poll_once(reader.next());
    assert!(pending.is_none());
    assert!(reader.buffered_capacity() <= 64);
}
