//! Frame layout shared by the reader and the writer (RFC 6455 section 5.2).

/// One received message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    /// A complete text message, reassembled from its fragments.
    Text(&'a str),
    /// A complete binary message, reassembled from its fragments.
    Binary(&'a [u8]),
    /// A ping; answer it with [`crate::WsWriter::pong`] and the same payload.
    Ping(&'a [u8]),
    /// A pong.
    Pong(&'a [u8]),
    /// The peer is closing. Answer with [`crate::WsWriter::close`] and drop
    /// the connection.
    Close(Option<CloseFrame<'a>>),
}

/// Status code and reason of a close frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseFrame<'a> {
    /// Close status code, such as 1000 or an application's 4000-4999 code.
    pub code: u16,
    /// UTF-8 reason, possibly empty.
    pub reason: &'a str,
}

/// Frame opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    pub(crate) const fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            0x0 => Some(Self::Continuation),
            0x1 => Some(Self::Text),
            0x2 => Some(Self::Binary),
            0x8 => Some(Self::Close),
            0x9 => Some(Self::Ping),
            0xA => Some(Self::Pong),
            _ => None,
        }
    }

    pub(crate) const fn bits(self) -> u8 {
        match self {
            Self::Continuation => 0x0,
            Self::Text => 0x1,
            Self::Binary => 0x2,
            Self::Close => 0x8,
            Self::Ping => 0x9,
            Self::Pong => 0xA,
        }
    }

    pub(crate) const fn is_control(self) -> bool {
        matches!(self, Self::Close | Self::Ping | Self::Pong)
    }
}

/// Longest payload a control frame may carry.
pub(crate) const MAX_CONTROL_PAYLOAD: usize = 125;

/// Longest frame header: two bytes, an eight-byte length, and a mask.
pub(crate) const MAX_HEADER: usize = 14;

/// Writes a frame header into `header` and returns its length.
pub(crate) fn encode_header(
    header: &mut [u8; MAX_HEADER],
    fin: bool,
    opcode: Opcode,
    payload_len: usize,
    mask: Option<[u8; 4]>,
) -> usize {
    let first = if fin { 0x80 } else { 0x00 } | opcode.bits();
    let mask_bit = if mask.is_some() { 0x80 } else { 0x00 };
    header[0] = first;
    let mut length = if payload_len < 126 {
        // fits seven bits
        header[1] = mask_bit | u8::try_from(payload_len).unwrap_or(0);
        2
    } else if let Ok(short) = u16::try_from(payload_len) {
        header[1] = mask_bit | 126;
        let [high, low] = short.to_be_bytes();
        header[2] = high;
        header[3] = low;
        4
    } else {
        header[1] = mask_bit | 127;
        let bytes = (payload_len as u64).to_be_bytes();
        for (slot, byte) in header.iter_mut().skip(2).zip(bytes) {
            *slot = byte;
        }
        10
    };
    if let Some(key) = mask {
        for (slot, byte) in header.iter_mut().skip(length).zip(key) {
            *slot = byte;
        }
        length = length.saturating_add(4);
    }
    length
}

/// Masks or unmasks `bytes` in place; `offset` is the position of
/// `bytes[0]` within the whole payload.
pub(crate) fn apply_mask(bytes: &mut [u8], key: [u8; 4], offset: usize) {
    for (index, byte) in bytes.iter_mut().enumerate() {
        let position = offset.wrapping_add(index) % 4;
        if let Some(mask) = key.get(position) {
            *byte ^= *mask;
        }
    }
}
