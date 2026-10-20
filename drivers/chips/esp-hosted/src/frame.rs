//! The ESP-Hosted payload header that frames every packet on the bus.
//!
//! Every transfer between host and co-processor is one or more packets, each
//! a 12-byte header (`struct esp_payload_header` in ESP-Hosted-MCU
//! `common/esp_hosted_header.h`, unchanged from 0.0.6 through 3.x as the "V1"
//! wire header) followed by its payload:
//!
//! | Byte | Field |
//! | --- | --- |
//! | 0 | interface type (bits 0-3), interface number (bits 4-7) |
//! | 1 | flags (bit 0: more fragments follow) |
//! | 2-3 | payload length, little-endian |
//! | 4-5 | payload offset from the header start, little-endian |
//! | 6-7 | byte-sum checksum of header and payload, little-endian |
//! | 8-9 | sequence number, little-endian |
//! | 10 | throttle command (bits 0-1) |
//! | 11 | packet type of the private or HCI interface |

use core::fmt;

/// Length of the payload header.
pub const HEADER_LEN: usize = 12;

/// Largest payload offset a co-processor may use: ESP-Hosted 3.x may pad the
/// header to align the payload (`EH_FRAME_V1_MAX_OFFSET_WITH_PADDING_BYTES`).
pub const MAX_OFFSET: usize = HEADER_LEN + 7;

/// Header flag: another fragment of the same message follows.
pub const FLAG_MORE_FRAGMENTS: u8 = 1 << 0;

/// The co-processor interface a packet belongs to (`esp_hosted_if_type_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interface {
    /// Ethernet frames of the Wi-Fi station.
    Station,
    /// Ethernet frames of the soft access point.
    AccessPoint,
    /// The RPC channel: TLV-framed protobuf requests, responses, and events.
    Serial,
    /// Bluetooth HCI packets.
    Hci,
    /// Transport set-up between host and co-processor.
    Private,
    /// Raw-throughput test traffic.
    Test,
    /// An interface this host does not know.
    Other(u8),
}

impl Interface {
    /// Returns the wire value.
    #[must_use]
    pub const fn value(self) -> u8 {
        match self {
            Self::Station => 1,
            Self::AccessPoint => 2,
            Self::Serial => 3,
            Self::Hci => 4,
            Self::Private => 5,
            Self::Test => 6,
            Self::Other(value) => value & 0x0f,
        }
    }

    /// Interprets a wire value.
    #[must_use]
    pub const fn from_value(value: u8) -> Self {
        match value & 0x0f {
            1 => Self::Station,
            2 => Self::AccessPoint,
            3 => Self::Serial,
            4 => Self::Hci,
            5 => Self::Private,
            6 => Self::Test,
            other => Self::Other(other),
        }
    }
}

/// One decoded payload header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The interface the payload belongs to.
    pub interface: Interface,
    /// The interface instance, always 0 on Wi-Fi co-processors.
    pub interface_number: u8,
    /// Header flags such as [`FLAG_MORE_FRAGMENTS`].
    pub flags: u8,
    /// Payload length in bytes.
    pub length: u16,
    /// Payload offset from the start of the header.
    pub offset: u16,
    /// Byte-sum checksum over header and payload.
    pub checksum: u16,
    /// Sender sequence number.
    pub sequence: u16,
    /// Packet type of the private or HCI interface.
    pub packet_type: u8,
}

impl Header {
    /// A header for `length` payload bytes on `interface`, payload right
    /// after the header, no checksum yet.
    #[must_use]
    pub const fn new(interface: Interface, length: u16, sequence: u16) -> Self {
        Self {
            interface,
            interface_number: 0,
            flags: 0,
            length,
            offset: HEADER_LEN as u16,
            checksum: 0,
            sequence,
            packet_type: 0,
        }
    }

    /// Encodes the header.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut bytes = [0; HEADER_LEN];
        bytes[0] = self.interface.value() | ((self.interface_number & 0x0f) << 4);
        bytes[1] = self.flags;
        bytes[2..4].copy_from_slice(&self.length.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.offset.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.checksum.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[11] = self.packet_type;
        bytes
    }

    /// Decodes the header at the start of `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::Truncated`] when fewer than [`HEADER_LEN`] bytes
    /// are given.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        let bytes: &[u8; HEADER_LEN] = bytes
            .get(..HEADER_LEN)
            .and_then(|header| header.try_into().ok())
            .ok_or(FrameError::Truncated)?;
        let word = |index: usize| u16::from_le_bytes([bytes[index], bytes[index + 1]]);
        Ok(Self {
            interface: Interface::from_value(bytes[0]),
            interface_number: bytes[0] >> 4,
            flags: bytes[1],
            length: word(2),
            offset: word(4),
            checksum: word(6),
            sequence: word(8),
            packet_type: bytes[11],
        })
    }

    /// Returns whether another fragment of the same message follows.
    #[must_use]
    pub const fn more_fragments(&self) -> bool {
        self.flags & FLAG_MORE_FRAGMENTS != 0
    }
}

/// The 16-bit byte sum ESP-Hosted uses as its packet checksum
/// (`compute_checksum` in `esp_hosted_transport.h`).
#[must_use]
pub fn checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0_u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

/// Writes one packet, header and payload, into `out` and returns its length.
///
/// The checksum is always filled in: a co-processor built without checksums
/// ignores the field, one built with them requires it.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] when the packet does not fit `out` or the
/// payload exceeds the 16-bit length field.
pub fn encode_packet(
    mut header: Header,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, FrameError> {
    let length = u16::try_from(payload.len()).map_err(|_| FrameError::TooLarge)?;
    let total = HEADER_LEN
        .checked_add(payload.len())
        .ok_or(FrameError::TooLarge)?;
    let packet = out.get_mut(..total).ok_or(FrameError::TooLarge)?;
    header.length = length;
    header.offset = HEADER_LEN as u16;
    header.checksum = 0;
    packet[..HEADER_LEN].copy_from_slice(&header.encode());
    packet[HEADER_LEN..].copy_from_slice(payload);
    let sum = checksum(packet);
    packet[6..8].copy_from_slice(&sum.to_le_bytes());
    Ok(total)
}

/// One packet found in received bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Its header.
    pub header: Header,
    /// Its payload.
    pub payload: &'a [u8],
}

/// How the co-processor lays packets out in one read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Packets follow each other directly (ESP-Hosted packet and streaming
    /// modes).
    Contiguous,
    /// Each packet starts on a 4-byte boundary and a zero-length header ends
    /// the transfer (ESP-Hosted 3.x SDIO software aggregation).
    Aligned,
}

/// What the parser found at the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// A complete packet, and how many bytes it occupies including any
    /// alignment padding after it.
    Packet(Packet<'a>, usize),
    /// The bytes end inside a packet; this many more are needed at least.
    Incomplete,
    /// A zero-length header: no more packets in this transfer.
    End,
}

/// Parses the packet at the start of `bytes`.
///
/// # Errors
///
/// Returns an error for a header whose offset is impossible or whose checksum
/// does not match while `verify_checksum` is set.
pub fn parse_packet(
    bytes: &[u8],
    layout: Layout,
    verify_checksum: bool,
) -> Result<Parsed<'_>, FrameError> {
    if bytes.len() < HEADER_LEN {
        return Ok(
            if bytes.iter().all(|byte| *byte == 0) && !bytes.is_empty() {
                Parsed::End
            } else {
                Parsed::Incomplete
            },
        );
    }
    let header = Header::decode(bytes)?;
    if header.length == 0 {
        return Ok(Parsed::End);
    }
    let offset = usize::from(header.offset);
    if !(HEADER_LEN..=MAX_OFFSET).contains(&offset) {
        return Err(FrameError::BadOffset(header.offset));
    }
    let end = offset
        .checked_add(usize::from(header.length))
        .ok_or(FrameError::TooLarge)?;
    let Some(packet) = bytes.get(..end) else {
        return Ok(Parsed::Incomplete);
    };
    if verify_checksum {
        let expected = packet[..6]
            .iter()
            .chain(&[0, 0])
            .chain(&packet[8..])
            .fold(0_u16, |sum, byte| sum.wrapping_add(u16::from(*byte)));
        if expected != header.checksum {
            return Err(FrameError::Checksum {
                expected,
                received: header.checksum,
            });
        }
    }
    let used = match layout {
        Layout::Contiguous => end,
        Layout::Aligned => end.checked_add(3).ok_or(FrameError::TooLarge)? & !3,
    };
    Ok(Parsed::Packet(
        Packet {
            header,
            payload: &packet[offset..],
        },
        used.min(bytes.len()),
    ))
}

/// A packet could not be framed or unframed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than a header.
    Truncated,
    /// The packet does not fit its buffer or length field.
    TooLarge,
    /// The payload offset is outside what any ESP-Hosted version sends.
    BadOffset(u16),
    /// The checksum does not match the bytes.
    Checksum {
        /// The sum of the received bytes.
        expected: u16,
        /// The sum the header carried.
        received: u16,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("packet shorter than its header"),
            Self::TooLarge => formatter.write_str("packet larger than its buffer"),
            Self::BadOffset(offset) => {
                write!(formatter, "packet payload offset {offset} is invalid")
            }
            Self::Checksum { expected, received } => write!(
                formatter,
                "packet checksum {received:#06x} does not match its bytes ({expected:#06x})"
            ),
        }
    }
}

impl core::error::Error for FrameError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_header_encodes_in_the_esp_payload_header_layout() {
        let mut header = Header::new(Interface::Serial, 0x0123, 0x0a0b);
        header.flags = FLAG_MORE_FRAGMENTS;
        header.checksum = 0xbeef;
        header.packet_type = 0x33;
        assert_eq!(
            header.encode(),
            [
                0x03, 0x01, 0x23, 0x01, 0x0c, 0x00, 0xef, 0xbe, 0x0b, 0x0a, 0x00, 0x33
            ]
        );
        assert_eq!(Header::decode(&header.encode()).unwrap(), header);
    }

    #[test]
    fn the_interface_number_sits_in_the_high_nibble() {
        let header = Header::decode(&[0x25, 0, 1, 0, 12, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(header.interface, Interface::Private);
        assert_eq!(header.interface_number, 2);
    }

    #[test]
    fn an_encoded_packet_carries_the_byte_sum_of_header_and_payload() {
        let mut out = [0; 32];
        let length =
            encode_packet(Header::new(Interface::Station, 0, 7), &[1, 2, 3], &mut out).unwrap();
        assert_eq!(length, 15);
        // Header bytes 01 00 03 00 0c 00 .. .. 07 00 00 00 sum to 0x17, the
        // payload to 6: 0x1d.
        assert_eq!(
            &out[..15],
            &[1, 0, 3, 0, 12, 0, 0x1d, 0, 7, 0, 0, 0, 1, 2, 3]
        );
        match parse_packet(&out[..15], Layout::Contiguous, true).unwrap() {
            Parsed::Packet(packet, used) => {
                assert_eq!(packet.payload, &[1, 2, 3]);
                assert_eq!(used, 15);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_corrupted_packet_fails_its_checksum() {
        let mut out = [0; 32];
        let length =
            encode_packet(Header::new(Interface::Station, 0, 1), &[9, 9], &mut out).unwrap();
        out[13] = 8;
        assert!(matches!(
            parse_packet(&out[..length], Layout::Contiguous, true),
            Err(FrameError::Checksum { .. })
        ));
        assert!(parse_packet(&out[..length], Layout::Contiguous, false).is_ok());
    }

    #[test]
    fn a_stream_yields_packets_back_to_back_and_aggregates_on_four_bytes() {
        let mut stream = [0; 64];
        let first = encode_packet(Header::new(Interface::Serial, 0, 0), &[1], &mut stream).unwrap();
        let second = encode_packet(
            Header::new(Interface::Station, 0, 0),
            &[2, 2],
            &mut stream[first..],
        )
        .unwrap();
        let Parsed::Packet(_, used) = parse_packet(&stream, Layout::Contiguous, true).unwrap()
        else {
            panic!("first packet")
        };
        assert_eq!(used, 13);
        let Parsed::Packet(packet, _) =
            parse_packet(&stream[used..], Layout::Contiguous, true).unwrap()
        else {
            panic!("second packet")
        };
        assert_eq!(packet.payload, &[2, 2]);
        assert_eq!(
            parse_packet(&stream[first + second..], Layout::Contiguous, true).unwrap(),
            Parsed::End
        );

        let Parsed::Packet(_, used) = parse_packet(&stream, Layout::Aligned, false).unwrap() else {
            panic!("aligned packet")
        };
        assert_eq!(used, 16);
    }

    #[test]
    fn a_packet_cut_short_asks_for_more_bytes() {
        let mut out = [0; 32];
        let length =
            encode_packet(Header::new(Interface::Station, 0, 0), &[5; 10], &mut out).unwrap();
        assert_eq!(
            parse_packet(&out[..length - 1], Layout::Contiguous, true).unwrap(),
            Parsed::Incomplete
        );
        assert_eq!(
            parse_packet(&out[..5], Layout::Contiguous, true).unwrap(),
            Parsed::Incomplete
        );
    }

    #[test]
    fn an_offset_no_version_sends_is_refused() {
        let bytes = [1, 0, 1, 0, 40, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            parse_packet(&bytes, Layout::Contiguous, false),
            Err(FrameError::BadOffset(40))
        );
    }
}
