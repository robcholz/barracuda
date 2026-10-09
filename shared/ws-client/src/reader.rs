//! The read half: buffers incoming bytes, parses frames, and reassembles
//! fragmented messages.

use alloc::vec::Vec;
use core::ops::Range;

use embedded_io_async::Read;

use crate::error::{HandshakeError, WsError};
use crate::frame::{CloseFrame, Message, Opcode, MAX_CONTROL_PAYLOAD, MAX_HEADER};
use crate::handshake::check_response;

/// Size limits of a [`WsReader`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest message payload accepted, whole or reassembled from fragments.
    pub max_message: usize,
    /// Read buffer size kept while idle. The buffer grows for a larger frame,
    /// up to `max_message` plus a frame header, and shrinks back afterwards.
    pub idle_buffer: usize,
    /// Largest HTTP response head accepted during the handshake.
    pub max_handshake: usize,
}

impl Limits {
    /// 64 KiB messages, a 1 KiB idle buffer, and a 2 KiB handshake head.
    pub const DEFAULT: Self = Self {
        max_message: 64 * 1024,
        idle_buffer: 1024,
        max_handshake: 2048,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A parsed frame header and where its payload lies in the buffer.
#[derive(Clone, Copy, Debug)]
struct Frame {
    fin: bool,
    opcode: Opcode,
    payload: (usize, usize),
    end: usize,
}

/// Outcome of looking for a frame in the buffered bytes.
enum Parse {
    Frame(Frame),
    /// More bytes are needed; the frame needs this many bytes from `start`
    /// once its header is known.
    Incomplete(Option<usize>),
}

/// What [`WsReader::next`] hands out, as positions so no borrow spans a read.
enum Ready {
    Buffered(Opcode, Range<usize>),
    Assembled(Opcode),
}

/// The read half of a WebSocket.
///
/// # Cancel safety
///
/// [`Self::next`] keeps every received byte in the reader between polls, so
/// dropping its future loses nothing when the underlying `read` is
/// cancel-safe (as `embassy-net` and split TLS read halves are).
pub struct WsReader<R> {
    inner: R,
    buffer: Vec<u8>,
    /// Bytes `start..end` of `buffer` are received and not yet consumed.
    start: usize,
    end: usize,
    /// Bytes of `buffer` the last returned message borrowed; consumed on the
    /// next call.
    consumed: usize,
    /// Opcode and payload of a fragmented message being reassembled.
    fragments: Option<(Opcode, Vec<u8>)>,
    /// Whether the last returned message was the reassembled one.
    assembled: bool,
    limits: Limits,
}

impl<R: Read> WsReader<R> {
    /// Wraps the read half of an already upgraded stream.
    ///
    /// [`crate::connect`] performs the handshake and returns one; use this
    /// only when the stream was upgraded by other means.
    #[must_use]
    pub fn new(inner: R, limits: Limits) -> Self {
        Self {
            inner,
            buffer: alloc::vec![0; limits.idle_buffer.max(MAX_HEADER)],
            start: 0,
            end: 0,
            consumed: 0,
            fragments: None,
            assembled: false,
            limits,
        }
    }

    /// Bytes the reader currently holds in RAM: the read buffer plus a
    /// fragmented message being reassembled.
    #[must_use]
    pub fn buffered_capacity(&self) -> usize {
        self.buffer.capacity().saturating_add(
            self.fragments
                .as_ref()
                .map_or(0, |(_opcode, payload)| payload.capacity()),
        )
    }

    /// Waits for the next message.
    ///
    /// Data messages arrive whole; fragments are reassembled first. Control
    /// frames (ping, pong, close) are returned as they arrive, also between
    /// the fragments of a data message.
    ///
    /// # Errors
    ///
    /// [`WsError::Io`] or [`WsError::Eof`] when the stream fails or ends,
    /// [`WsError::Protocol`] when the server breaks the framing rules,
    /// [`WsError::TooLarge`] past [`Limits::max_message`], and
    /// [`WsError::InvalidUtf8`] for a text message that is not UTF-8.
    ///
    /// # Cancel safety
    ///
    /// Cancel-safe when the underlying `read` is.
    pub async fn next(&mut self) -> Result<Message<'_>, WsError> {
        self.release();
        let ready = loop {
            match self.parse()? {
                Parse::Frame(frame) => {
                    if let Some(ready) = self.accept(frame)? {
                        break ready;
                    }
                }
                Parse::Incomplete(needed) => self.fill(needed).await?,
            }
        };
        self.message(ready)
    }

    /// Reads the handshake response head and checks it; bytes after it stay
    /// buffered.
    pub(crate) async fn read_handshake(&mut self, expected_accept: &[u8]) -> Result<(), WsError> {
        loop {
            let received = self.buffer.get(..self.end).unwrap_or_default();
            if let Some(head_end) = find(received, b"\r\n\r\n") {
                let head = received.get(..head_end).unwrap_or_default();
                check_response(head, expected_accept)?;
                self.start = head_end.saturating_add(4);
                return Ok(());
            }
            if self.end >= self.limits.max_handshake {
                return Err(HandshakeError::HeadersTooLarge.into());
            }
            let wanted = self.end.saturating_add(256).min(self.limits.max_handshake);
            self.fill(Some(wanted)).await?;
        }
    }

    /// Consumes what the previous message borrowed and shrinks an enlarged
    /// buffer once it is empty.
    fn release(&mut self) {
        self.start = self.start.saturating_add(self.consumed).min(self.end);
        self.consumed = 0;
        if self.assembled {
            self.assembled = false;
            self.fragments = None;
        }
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
            let idle = self.limits.idle_buffer.max(MAX_HEADER);
            if self.buffer.len() > idle {
                self.buffer.truncate(idle);
                self.buffer.shrink_to_fit();
            }
        }
    }

    /// Reads more bytes, making room for `needed` bytes from `start` first.
    async fn fill(&mut self, needed: Option<usize>) -> Result<(), WsError> {
        if self.start > 0 {
            self.buffer.copy_within(self.start..self.end, 0);
            self.end = self.end.saturating_sub(self.start);
            self.start = 0;
        }
        let wanted = needed.unwrap_or(0).max(self.end.saturating_add(1));
        if wanted > self.buffer.len() {
            self.buffer
                .reserve_exact(wanted.saturating_sub(self.buffer.len()));
            self.buffer.resize(wanted, 0);
        }
        let free = self.buffer.get_mut(self.end..).unwrap_or_default();
        let read = self
            .inner
            .read(free)
            .await
            .map_err(|error| WsError::io(&error))?;
        if read == 0 {
            return Err(WsError::Eof);
        }
        self.end = self.end.saturating_add(read).min(self.buffer.len());
        Ok(())
    }

    /// Parses the frame at `start`, if all of it is buffered.
    fn parse(&self) -> Result<Parse, WsError> {
        let bytes = self.buffer.get(self.start..self.end).unwrap_or_default();
        let (Some(&first), Some(&second)) = (bytes.first(), bytes.get(1)) else {
            return Ok(Parse::Incomplete(None));
        };
        if first & 0x70 != 0 {
            return Err(WsError::Protocol("reserved bits set without an extension"));
        }
        let fin = first & 0x80 != 0;
        let opcode = Opcode::from_bits(first & 0x0F).ok_or(WsError::Protocol("unknown opcode"))?;
        if second & 0x80 != 0 {
            return Err(WsError::Protocol("server frames must not be masked"));
        }
        let (header, length) = match second & 0x7F {
            126 => {
                let Some(&[high, low]) = bytes.get(2..4) else {
                    return Ok(Parse::Incomplete(None));
                };
                let length = u16::from_be_bytes([high, low]);
                if length < 126 {
                    return Err(WsError::Protocol("length not minimally encoded"));
                }
                (4_usize, u64::from(length))
            }
            127 => {
                let Some(length) = bytes.get(2..10) else {
                    return Ok(Parse::Incomplete(None));
                };
                let mut be = [0_u8; 8];
                be.copy_from_slice(length);
                let length = u64::from_be_bytes(be);
                if length >> 63 != 0 {
                    return Err(WsError::Protocol("length has its top bit set"));
                }
                if length <= u64::from(u16::MAX) {
                    return Err(WsError::Protocol("length not minimally encoded"));
                }
                (10_usize, length)
            }
            short => (2_usize, u64::from(short)),
        };
        if opcode.is_control() {
            if !fin {
                return Err(WsError::Protocol("fragmented control frame"));
            }
            if length > MAX_CONTROL_PAYLOAD as u64 {
                return Err(WsError::ControlTooLong);
            }
        }
        let length = usize::try_from(length).map_err(|_| WsError::TooLarge)?;
        if length > self.limits.max_message {
            return Err(WsError::TooLarge);
        }
        let total = header.checked_add(length).ok_or(WsError::TooLarge)?;
        if bytes.len() < total {
            return Ok(Parse::Incomplete(Some(total)));
        }
        let payload_start = self.start.saturating_add(header);
        Ok(Parse::Frame(Frame {
            fin,
            opcode,
            payload: (payload_start, payload_start.saturating_add(length)),
            end: self.start.saturating_add(total),
        }))
    }

    /// Applies a complete frame, returning what to hand out, if anything.
    fn accept(&mut self, frame: Frame) -> Result<Option<Ready>, WsError> {
        let (payload_start, payload_end) = frame.payload;
        if frame.opcode.is_control() {
            self.consumed = frame.end.saturating_sub(self.start);
            return Ok(Some(Ready::Buffered(
                frame.opcode,
                payload_start..payload_end,
            )));
        }
        match (frame.opcode, self.fragments.is_some()) {
            (Opcode::Continuation, false) => {
                Err(WsError::Protocol("continuation without a message"))
            }
            (Opcode::Text | Opcode::Binary, true) => {
                Err(WsError::Protocol("new message inside a fragmented one"))
            }
            (opcode, false) if frame.fin => {
                self.consumed = frame.end.saturating_sub(self.start);
                Ok(Some(Ready::Buffered(opcode, payload_start..payload_end)))
            }
            (opcode, _) => {
                let payload = self
                    .buffer
                    .get(payload_start..payload_end)
                    .unwrap_or_default();
                let (kind, assembled) = self.fragments.get_or_insert_with(|| (opcode, Vec::new()));
                if assembled.len().saturating_add(payload.len()) > self.limits.max_message {
                    self.fragments = None;
                    return Err(WsError::TooLarge);
                }
                assembled.extend_from_slice(payload);
                let kind = *kind;
                self.start = frame.end;
                if frame.fin {
                    self.assembled = true;
                    return Ok(Some(Ready::Assembled(kind)));
                }
                Ok(None)
            }
        }
    }

    fn message(&self, ready: Ready) -> Result<Message<'_>, WsError> {
        let (opcode, payload) = match ready {
            Ready::Buffered(opcode, range) => (opcode, self.buffer.get(range).unwrap_or_default()),
            Ready::Assembled(opcode) => (
                opcode,
                self.fragments
                    .as_ref()
                    .map(|(_kind, payload)| payload.as_slice())
                    .unwrap_or_default(),
            ),
        };
        match opcode {
            Opcode::Text => core::str::from_utf8(payload)
                .map(Message::Text)
                .map_err(|_| WsError::InvalidUtf8),
            Opcode::Binary | Opcode::Continuation => Ok(Message::Binary(payload)),
            Opcode::Ping => Ok(Message::Ping(payload)),
            Opcode::Pong => Ok(Message::Pong(payload)),
            Opcode::Close => close_frame(payload).map(Message::Close),
        }
    }
}

fn close_frame(payload: &[u8]) -> Result<Option<CloseFrame<'_>>, WsError> {
    match payload {
        [] => Ok(None),
        [_] => Err(WsError::Protocol("close payload of one byte")),
        [high, low, reason @ ..] => Ok(Some(CloseFrame {
            code: u16::from_be_bytes([*high, *low]),
            reason: core::str::from_utf8(reason).map_err(|_| WsError::InvalidUtf8)?,
        })),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
