//! ISO/IEC 14443-A card activation and NFC Forum Type 2 `READ`, over any
//! reader chip.
//!
//! A reader chip driver implements [`Transceiver`]: the three frame shapes
//! activation uses. [`activate`] then wakes a card and runs the cascade of
//! ISO/IEC 14443-3 clause 6.5.3: per cascade level an anticollision command
//! (`SEL`, `NVB = 20h`) returns four UID bytes and their BCC, and a `SELECT`
//! (`SEL`, `NVB = 70h`, the five bytes, CRC_A) returns the SAK, whose bit 3
//! says another level follows. A UID whose first byte at a level is the
//! cascade tag `88h` continues at the next level. Collisions between several
//! cards are reported, not resolved.

#![no_std]

use core::future::Future;

/// Command codes and constants of ISO/IEC 14443-3 type A.
pub mod command {
    /// Request, type A (seven bits).
    pub const REQA: u8 = 0x26;
    /// Wake-up, type A (seven bits).
    pub const WUPA: u8 = 0x52;
    /// Select code of cascade level 1.
    pub const SEL_CL1: u8 = 0x93;
    /// Select code of cascade level 2.
    pub const SEL_CL2: u8 = 0x95;
    /// Select code of cascade level 3.
    pub const SEL_CL3: u8 = 0x97;
    /// Number of valid bits of an anticollision command: two whole bytes.
    pub const NVB_ANTICOLLISION: u8 = 0x20;
    /// Number of valid bits of a select command: seven whole bytes.
    pub const NVB_SELECT: u8 = 0x70;
    /// The cascade tag that marks a UID continuing at the next level.
    pub const CASCADE_TAG: u8 = 0x88;
    /// SAK bit: the UID is not complete.
    pub const SAK_CASCADE: u8 = 0x04;
    /// Halt, type A (`HLTA`, sent with `00h` and CRC_A).
    pub const HLTA: u8 = 0x50;
    /// NFC Forum Type 2 `READ`: sixteen bytes from a page.
    pub const READ: u8 = 0x30;
}

/// Bytes one Type 2 `READ` returns.
pub const READ_BYTES: usize = 16;

/// Longest UID: three cascade levels.
pub const MAX_UID_BYTES: usize = 10;

/// Computes the CRC_A of ISO/IEC 14443-3 Annex B (polynomial `8408h`
/// reflected, preset `6363h`), low byte first as transmitted.
#[must_use]
pub fn crc_a(data: &[u8]) -> [u8; 2] {
    let mut crc: u16 = 0x6363;
    for &byte in data {
        let mut value = byte ^ crc.to_le_bytes()[0];
        value ^= value << 4;
        let value = u16::from(value);
        crc = (crc >> 8) ^ (value << 8) ^ (value << 3) ^ (value >> 4);
    }
    crc.to_le_bytes()
}

/// Returns whether `frame` ends with the CRC_A of the bytes before it.
#[must_use]
pub fn crc_a_matches(frame: &[u8]) -> bool {
    match frame.len().checked_sub(2) {
        Some(split) => frame.get(split..) == Some(&crc_a(frame.get(..split).unwrap_or(&[]))[..]),
        None => false,
    }
}

/// The three frame shapes card activation needs from a reader chip. Each
/// returns `None` when no card answered within the frame waiting time.
pub trait Transceiver {
    /// Reader chip or bus failure.
    type Error;

    /// Sends `REQA`, or `WUPA` when `wake` is set, as a seven-bit short
    /// frame and returns the ATQA, first byte received in the low half.
    fn request(&mut self, wake: bool) -> impl Future<Output = Result<Option<u16>, Self::Error>>;

    /// Sends the anticollision command `[sel, 20h]` without CRC and returns
    /// the five bytes answered: four UID bytes and their BCC.
    fn anticollision(
        &mut self,
        sel: u8,
    ) -> impl Future<Output = Result<Option<[u8; 5]>, Self::Error>>;

    /// Sends `frame` with its CRC_A appended and copies the answer, its
    /// CRC_A checked and removed, into `reply`; returns the answer's length.
    fn transceive_crc(
        &mut self,
        frame: &[u8],
        reply: &mut [u8],
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>>;
}

/// Activation or transfer failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The reader chip or its bus failed.
    Reader(E),
    /// A card stopped answering in the middle of activation or a read.
    NoAnswer,
    /// Several cards answered at once, or a UID's check byte was wrong.
    Collision,
    /// A card answered outside the protocol: a short SAK, a cascade tag
    /// where none may be, or more than three levels.
    Protocol,
}

/// A selected card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activated {
    uid: [u8; MAX_UID_BYTES],
    uid_len: usize,
    atqa: u16,
    sak: u8,
}

impl Activated {
    /// Returns the UID: four, seven, or ten bytes.
    #[must_use]
    pub fn uid(&self) -> &[u8] {
        self.uid.get(..self.uid_len).unwrap_or(&[])
    }

    /// Returns the answer to request.
    #[must_use]
    pub const fn atqa(&self) -> u16 {
        self.atqa
    }

    /// Returns the final select acknowledge.
    #[must_use]
    pub const fn sak(&self) -> u8 {
        self.sak
    }
}

/// Wakes a card in the field and selects it: `REQA`, then `WUPA` when no
/// idle card answers (as M5Stack's ST25R3916 driver does), then the cascade.
/// Returns `None` when no card answers at all.
///
/// # Errors
///
/// Returns the reader's failure, or a protocol failure once a card answered.
pub async fn activate<T: Transceiver>(
    reader: &mut T,
) -> Result<Option<Activated>, Error<T::Error>> {
    let atqa = match reader.request(false).await.map_err(Error::Reader)? {
        Some(atqa) => atqa,
        None => match reader.request(true).await.map_err(Error::Reader)? {
            Some(atqa) => atqa,
            None => return Ok(None),
        },
    };
    let mut uid = [0; MAX_UID_BYTES];
    let mut uid_len: usize = 0;
    for (level, sel) in [command::SEL_CL1, command::SEL_CL2, command::SEL_CL3]
        .into_iter()
        .enumerate()
    {
        let answer = reader
            .anticollision(sel)
            .await
            .map_err(Error::Reader)?
            .ok_or(Error::NoAnswer)?;
        let [u0, u1, u2, u3, bcc] = answer;
        if u0 ^ u1 ^ u2 ^ u3 != bcc {
            return Err(Error::Collision);
        }
        let select = [sel, command::NVB_SELECT, u0, u1, u2, u3, bcc];
        let mut sak = [0; 1];
        let len = reader
            .transceive_crc(&select, &mut sak)
            .await
            .map_err(Error::Reader)?
            .ok_or(Error::NoAnswer)?;
        if len != 1 {
            return Err(Error::Protocol);
        }
        let last_level = level == 2;
        let continues = sak[0] & command::SAK_CASCADE != 0;
        let bytes: &[u8] = if continues {
            // The cascade tag is not part of the UID.
            if u0 != command::CASCADE_TAG || last_level {
                return Err(Error::Protocol);
            }
            &answer[1..4]
        } else {
            if u0 == command::CASCADE_TAG {
                return Err(Error::Protocol);
            }
            &answer[..4]
        };
        let end = uid_len.checked_add(bytes.len()).ok_or(Error::Protocol)?;
        uid.get_mut(uid_len..end)
            .ok_or(Error::Protocol)?
            .copy_from_slice(bytes);
        uid_len = end;
        if !continues {
            return Ok(Some(Activated {
                uid,
                uid_len,
                atqa,
                sak: sak[0],
            }));
        }
    }
    Err(Error::Protocol)
}

/// Reads [`READ_BYTES`] bytes starting at `page` of the selected card.
///
/// # Errors
///
/// Returns the reader's failure, [`Error::NoAnswer`] when the card does not
/// answer (a MIFARE Classic card without authentication), or
/// [`Error::Protocol`] for a short answer, such as a four-bit NAK.
pub async fn read<T: Transceiver>(
    reader: &mut T,
    page: u8,
) -> Result<[u8; READ_BYTES], Error<T::Error>> {
    let mut data = [0; READ_BYTES];
    let len = reader
        .transceive_crc(&[command::READ, page], &mut data)
        .await
        .map_err(Error::Reader)?
        .ok_or(Error::NoAnswer)?;
    if len == READ_BYTES {
        Ok(data)
    } else {
        Err(Error::Protocol)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects, clippy::panic)]

    extern crate std;

    use std::vec::Vec;

    use embassy_futures::block_on;

    use super::{Error, Transceiver, activate, command, crc_a, crc_a_matches, read};

    #[test]
    fn crc_a_matches_the_standard_examples() {
        // ISO/IEC 14443-3 Annex B: HLTA `50 00` carries `57 CD`.
        assert_eq!(crc_a(&[command::HLTA, 0x00]), [0x57, 0xcd]);
        // A Type 2 READ of page 0 carries `02 A8`.
        assert_eq!(crc_a(&[command::READ, 0x00]), [0x02, 0xa8]);
        assert!(crc_a_matches(&[0x50, 0x00, 0x57, 0xcd]));
        assert!(!crc_a_matches(&[0x50, 0x00, 0x57, 0xce]));
        assert!(!crc_a_matches(&[0x57]));
    }

    /// A card in the field, answering by its cascade levels.
    struct Card {
        idle_answers: bool,
        levels: Vec<[u8; 5]>,
        saks: Vec<u8>,
        memory: [u8; 64],
        log: Vec<Vec<u8>>,
    }

    impl Card {
        fn single(uid: [u8; 4], sak: u8) -> Self {
            let bcc = uid[0] ^ uid[1] ^ uid[2] ^ uid[3];
            Self {
                idle_answers: true,
                levels: std::vec![[uid[0], uid[1], uid[2], uid[3], bcc]],
                saks: std::vec![sak],
                memory: core::array::from_fn(|index| index as u8),
                log: Vec::new(),
            }
        }

        fn double(uid: [u8; 7]) -> Self {
            let first = [command::CASCADE_TAG, uid[0], uid[1], uid[2]];
            let second = [uid[3], uid[4], uid[5], uid[6]];
            let bcc = |part: [u8; 4]| part[0] ^ part[1] ^ part[2] ^ part[3];
            Self {
                idle_answers: true,
                levels: std::vec![
                    [first[0], first[1], first[2], first[3], bcc(first)],
                    [second[0], second[1], second[2], second[3], bcc(second)],
                ],
                saks: std::vec![0x04, 0x00],
                memory: [0; 64],
                log: Vec::new(),
            }
        }
    }

    fn level(sel: u8) -> usize {
        match sel {
            command::SEL_CL1 => 0,
            command::SEL_CL2 => 1,
            _ => 2,
        }
    }

    impl Transceiver for Card {
        type Error = ();

        async fn request(&mut self, wake: bool) -> Result<Option<u16>, ()> {
            self.log
                .push(std::vec![if wake { command::WUPA } else { command::REQA }]);
            Ok((wake || self.idle_answers).then_some(0x0044))
        }

        async fn anticollision(&mut self, sel: u8) -> Result<Option<[u8; 5]>, ()> {
            self.log.push(std::vec![sel, command::NVB_ANTICOLLISION]);
            Ok(self.levels.get(level(sel)).copied())
        }

        async fn transceive_crc(
            &mut self,
            frame: &[u8],
            reply: &mut [u8],
        ) -> Result<Option<usize>, ()> {
            self.log.push(frame.to_vec());
            match frame[0] {
                command::READ => {
                    let start = usize::from(frame[1]) * 4;
                    reply.copy_from_slice(&self.memory[start..start + 16]);
                    Ok(Some(16))
                }
                sel => {
                    let Some(sak) = self.saks.get(level(sel)) else {
                        return Ok(None);
                    };
                    reply[0] = *sak;
                    Ok(Some(1))
                }
            }
        }
    }

    #[test]
    fn selects_a_single_size_uid() {
        let mut card = Card::single([0xde, 0xad, 0xbe, 0xef], 0x08);
        let activated = block_on(activate(&mut card))
            .expect("activate")
            .expect("a card");
        assert_eq!(activated.uid(), [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(activated.atqa(), 0x0044);
        assert_eq!(activated.sak(), 0x08);
        assert_eq!(
            card.log[2],
            [
                0x93,
                0x70,
                0xde,
                0xad,
                0xbe,
                0xef,
                0xde ^ 0xad ^ 0xbe ^ 0xef
            ]
        );
    }

    #[test]
    fn follows_the_cascade_tag_to_a_double_size_uid() {
        let mut card = Card::double([0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let activated = block_on(activate(&mut card))
            .expect("activate")
            .expect("a card");
        assert_eq!(activated.uid(), [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        assert_eq!(activated.sak(), 0x00);
        let sels: Vec<u8> = card.log.iter().map(|frame| frame[0]).collect();
        assert_eq!(sels, [0x26, 0x93, 0x93, 0x95, 0x95]);
    }

    #[test]
    fn wakes_a_halted_card_and_reports_an_empty_field() {
        let mut card = Card::single([1, 2, 3, 4], 0x00);
        card.idle_answers = false;
        assert!(block_on(activate(&mut card)).expect("activate").is_some());
        assert_eq!(card.log[0], [command::REQA]);
        assert_eq!(card.log[1], [command::WUPA]);

        struct Empty;
        impl Transceiver for Empty {
            type Error = ();
            async fn request(&mut self, _wake: bool) -> Result<Option<u16>, ()> {
                Ok(None)
            }
            async fn anticollision(&mut self, _sel: u8) -> Result<Option<[u8; 5]>, ()> {
                panic!("no card to resolve")
            }
            async fn transceive_crc(
                &mut self,
                _frame: &[u8],
                _reply: &mut [u8],
            ) -> Result<Option<usize>, ()> {
                panic!("no card to select")
            }
        }
        assert_eq!(block_on(activate(&mut Empty)), Ok(None));
    }

    #[test]
    fn reports_a_wrong_check_byte_as_a_collision() {
        let mut card = Card::single([1, 2, 3, 4], 0x00);
        card.levels[0][4] ^= 0xff;
        assert_eq!(block_on(activate(&mut card)), Err(Error::Collision));
    }

    #[test]
    fn reads_sixteen_bytes_of_a_type_2_tag() {
        let mut card = Card::single([1, 2, 3, 4], 0x00);
        let data = block_on(read(&mut card, 2)).expect("read");
        assert_eq!(data[0], 8);
        assert_eq!(data[15], 23);
        assert_eq!(card.log[0], [command::READ, 2]);
    }
}
