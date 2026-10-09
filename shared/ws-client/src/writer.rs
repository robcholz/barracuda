//! The write half: masks and frames outgoing messages.

use alloc::vec::Vec;

use embedded_io_async::Write;

use crate::error::WsError;
use crate::frame::{apply_mask, encode_header, Opcode, MAX_CONTROL_PAYLOAD, MAX_HEADER};

/// Source of the four-byte masking key of each client frame.
///
/// RFC 6455 requires a fresh, unpredictable key per frame; pass the
/// Platform's entropy.
pub trait MaskSource {
    /// Returns the key for the next frame.
    fn mask(&mut self) -> [u8; 4];
}

impl<F: FnMut() -> [u8; 4]> MaskSource for F {
    fn mask(&mut self) -> [u8; 4] {
        self()
    }
}

/// The write half of a WebSocket.
///
/// Every frame is written to completion and flushed. The methods are not
/// cancel-safe: dropping one mid-frame corrupts the stream, so drop the
/// connection with it.
pub struct WsWriter<W, M> {
    inner: W,
    mask: M,
}

impl<W: Write, M: MaskSource> WsWriter<W, M> {
    /// Wraps the write half of an already upgraded stream.
    #[must_use]
    pub const fn new(inner: W, mask: M) -> Self {
        Self { inner, mask }
    }

    /// Sends one unfragmented text message.
    ///
    /// # Errors
    ///
    /// [`WsError::Io`] when writing fails.
    pub async fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.frame(Opcode::Text, text.as_bytes()).await
    }

    /// Sends one unfragmented binary message.
    ///
    /// # Errors
    ///
    /// [`WsError::Io`] when writing fails.
    pub async fn send_binary(&mut self, bytes: &[u8]) -> Result<(), WsError> {
        self.frame(Opcode::Binary, bytes).await
    }

    /// Sends a ping with up to 125 bytes of payload.
    ///
    /// # Errors
    ///
    /// [`WsError::ControlTooLong`] for a longer payload, [`WsError::Io`]
    /// when writing fails.
    pub async fn ping(&mut self, payload: &[u8]) -> Result<(), WsError> {
        self.control(Opcode::Ping, payload).await
    }

    /// Answers a ping with its payload.
    ///
    /// # Errors
    ///
    /// [`WsError::ControlTooLong`] for a payload over 125 bytes,
    /// [`WsError::Io`] when writing fails.
    pub async fn pong(&mut self, payload: &[u8]) -> Result<(), WsError> {
        self.control(Opcode::Pong, payload).await
    }

    /// Sends a close frame with `code` and a reason of at most 123 bytes
    /// (longer reasons are cut at a character boundary).
    ///
    /// # Errors
    ///
    /// [`WsError::Io`] when writing fails.
    pub async fn close(&mut self, code: u16, reason: &str) -> Result<(), WsError> {
        let mut payload = [0_u8; MAX_CONTROL_PAYLOAD];
        let [high, low] = code.to_be_bytes();
        payload[0] = high;
        payload[1] = low;
        let mut cut = reason.len().min(MAX_CONTROL_PAYLOAD.saturating_sub(2));
        while !reason.is_char_boundary(cut) {
            cut = cut.saturating_sub(1);
        }
        let reason = reason.as_bytes().get(..cut).unwrap_or_default();
        for (slot, byte) in payload.iter_mut().skip(2).zip(reason) {
            *slot = *byte;
        }
        let length = cut.saturating_add(2);
        self.control(Opcode::Close, payload.get(..length).unwrap_or_default())
            .await
    }

    /// Mutable access to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    async fn control(&mut self, opcode: Opcode, payload: &[u8]) -> Result<(), WsError> {
        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(WsError::ControlTooLong);
        }
        self.frame(opcode, payload).await
    }

    /// Writes one masked frame with a single write, so a TLS stream sends it
    /// as one record.
    async fn frame(&mut self, opcode: Opcode, payload: &[u8]) -> Result<(), WsError> {
        let key = self.mask.mask();
        let mut header = [0_u8; MAX_HEADER];
        let length = encode_header(&mut header, true, opcode, payload.len(), Some(key));
        let mut frame = Vec::with_capacity(length.saturating_add(payload.len()));
        frame.extend_from_slice(header.get(..length).unwrap_or_default());
        frame.extend_from_slice(payload);
        apply_mask(frame.get_mut(length..).unwrap_or_default(), key, 0);
        self.write(&frame).await?;
        drop(frame);
        self.inner
            .flush()
            .await
            .map_err(|error| WsError::io(&error))
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), WsError> {
        self.inner
            .write_all(bytes)
            .await
            .map_err(|error| WsError::io(&error))
    }
}
