//! Encoder and decoder for NEC infrared remote-control frames.
//!
//! An NEC frame is a 9 ms leader mark and a 4.5 ms space, then 32 bits sent
//! least significant first, then a closing 562.5 µs mark. Every bit is a
//! 562.5 µs mark followed by a 562.5 µs space for a zero or a 1687.5 µs space
//! for a one. The bits are the address, the inverted address, the command,
//! and the inverted command; extended NEC replaces the inverted address with
//! the address's high byte. A held button repeats a short frame: the leader
//! mark, a 2.25 ms space, and the closing mark.
//!
//! The timings are those of NEC's µPD6121 transmission format, as the
//! ESP-IDF `ir_nec` example encodes them. Times are whole microseconds; a
//! decoder accepts each within a quarter of its nominal length.

#![no_std]

/// Leader mark.
pub const LEADER_MARK_US: u16 = 9_000;
/// Space after the leader of a command frame.
pub const LEADER_SPACE_US: u16 = 4_500;
/// Space after the leader of a repeat frame.
pub const REPEAT_SPACE_US: u16 = 2_250;
/// Mark of every bit and of the closing burst.
pub const BIT_MARK_US: u16 = 562;
/// Space of a zero bit.
pub const ZERO_SPACE_US: u16 = 562;
/// Space of a one bit.
pub const ONE_SPACE_US: u16 = 1_687;

/// Bursts in a command frame: the leader, 32 bits, and the closing mark.
pub const FRAME_BURSTS: usize = 34;
/// Bursts in a repeat frame: the leader and the closing mark.
pub const REPEAT_BURSTS: usize = 2;

/// One mark and the space after it, in microseconds; a closing burst's space
/// is zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Burst {
    /// How long the carrier is on.
    pub mark_us: u16,
    /// How long it stays off afterwards.
    pub space_us: u16,
}

impl Burst {
    /// Creates one burst.
    #[must_use]
    pub const fn new(mark_us: u16, space_us: u16) -> Self {
        Self { mark_us, space_us }
    }
}

/// A frame a decoder recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A command for an eight-bit address, sent with its inverse.
    Standard {
        /// The address.
        address: u8,
        /// The command.
        command: u8,
    },
    /// A command for a sixteen-bit address.
    Extended {
        /// The address, low byte sent first.
        address: u16,
        /// The command.
        command: u8,
    },
    /// The repeat code of a held button.
    Repeat,
}

/// Encodes a standard frame: `address`, its inverse, `command`, its inverse.
#[must_use]
pub fn encode_standard(address: u8, command: u8) -> [Burst; FRAME_BURSTS] {
    encode_bits(u32::from_le_bytes([address, !address, command, !command]))
}

/// Encodes an extended frame: the sixteen address bits, low byte first, then
/// `command` and its inverse.
#[must_use]
pub fn encode_extended(address: u16, command: u8) -> [Burst; FRAME_BURSTS] {
    let [low, high] = address.to_le_bytes();
    encode_bits(u32::from_le_bytes([low, high, command, !command]))
}

/// Encodes the repeat frame.
#[must_use]
pub const fn encode_repeat() -> [Burst; REPEAT_BURSTS] {
    [
        Burst::new(LEADER_MARK_US, REPEAT_SPACE_US),
        Burst::new(BIT_MARK_US, 0),
    ]
}

fn encode_bits(bits: u32) -> [Burst; FRAME_BURSTS] {
    let mut frame = [Burst::new(BIT_MARK_US, 0); FRAME_BURSTS];
    frame[0] = Burst::new(LEADER_MARK_US, LEADER_SPACE_US);
    for (index, burst) in frame[1..=32].iter_mut().enumerate() {
        let one = (bits >> index) & 1 == 1;
        *burst = Burst::new(BIT_MARK_US, if one { ONE_SPACE_US } else { ZERO_SPACE_US });
    }
    frame
}

/// Decodes a received frame from its bursts.
///
/// A standard frame whose address and command do not match their inverses,
/// and an extended frame whose command does not, is not NEC and decodes to
/// `None`. The space after the closing mark is ignored, as a receiver
/// reports it as the idle gap that ended the reception.
#[must_use]
pub fn decode(bursts: &[Burst]) -> Option<Frame> {
    let (leader, rest) = bursts.split_first()?;
    if !near(leader.mark_us, LEADER_MARK_US) {
        return None;
    }
    if near(leader.space_us, REPEAT_SPACE_US) {
        return rest
            .first()
            .filter(|closing| near(closing.mark_us, BIT_MARK_US))
            .map(|_| Frame::Repeat);
    }
    if !near(leader.space_us, LEADER_SPACE_US) || rest.len() < 33 {
        return None;
    }
    let mut bits = 0u32;
    for (index, burst) in rest[..32].iter().enumerate() {
        if !near(burst.mark_us, BIT_MARK_US) {
            return None;
        }
        if near(burst.space_us, ONE_SPACE_US) {
            bits |= 1 << index;
        } else if !near(burst.space_us, ZERO_SPACE_US) {
            return None;
        }
    }
    if !near(rest[32].mark_us, BIT_MARK_US) {
        return None;
    }
    let [low, high, command, inverse] = bits.to_le_bytes();
    if command != !inverse {
        return None;
    }
    Some(if high == !low {
        Frame::Standard {
            address: low,
            command,
        }
    } else {
        Frame::Extended {
            address: u16::from_le_bytes([low, high]),
            command,
        }
    })
}

/// Whether `measured` lies within a quarter of `nominal`.
const fn near(measured: u16, nominal: u16) -> bool {
    let tolerance = nominal / 4;
    measured >= nominal.saturating_sub(tolerance) && measured <= nominal.saturating_add(tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_frame_sends_address_command_and_inverses_lsb_first() {
        let frame = encode_standard(0x04, 0x08);
        assert_eq!(frame[0], Burst::new(9_000, 4_500));
        // Address 0x04: bit 2 set.
        assert_eq!(frame[1].space_us, ZERO_SPACE_US);
        assert_eq!(frame[3].space_us, ONE_SPACE_US);
        // Inverted address 0xFB: bit 2 clear, the rest set.
        assert_eq!(frame[9].space_us, ONE_SPACE_US);
        assert_eq!(frame[11].space_us, ZERO_SPACE_US);
        // Command 0x08: bit 3 set.
        assert_eq!(frame[20].space_us, ONE_SPACE_US);
        assert_eq!(frame[33], Burst::new(BIT_MARK_US, 0));
    }

    #[test]
    fn frames_decode_to_what_was_encoded() {
        assert_eq!(
            decode(&encode_standard(0x10, 0xa5)),
            Some(Frame::Standard {
                address: 0x10,
                command: 0xa5
            })
        );
        assert_eq!(
            decode(&encode_extended(0x1234, 0x42)),
            Some(Frame::Extended {
                address: 0x1234,
                command: 0x42
            })
        );
        assert_eq!(decode(&encode_repeat()), Some(Frame::Repeat));
    }

    #[test]
    fn decoding_tolerates_receiver_timing_error() {
        let mut frame = encode_standard(0x01, 0x02);
        for burst in &mut frame {
            burst.mark_us = burst.mark_us + burst.mark_us / 10;
            burst.space_us = burst.space_us - burst.space_us / 10;
        }
        // A receiver reports the idle gap after the closing mark.
        frame[33].space_us = 12_000;
        assert_eq!(
            decode(&frame),
            Some(Frame::Standard {
                address: 0x01,
                command: 0x02
            })
        );
    }

    #[test]
    fn foreign_and_corrupt_frames_are_not_nec() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[Burst::new(2_400, 600)]), None);
        let mut frame = encode_standard(0x01, 0x02);
        // Flip one command bit so it no longer matches its inverse.
        frame[17].space_us = ONE_SPACE_US;
        assert_eq!(decode(&frame), None);
        let truncated = encode_standard(0x01, 0x02);
        assert_eq!(decode(&truncated[..20]), None);
    }
}
