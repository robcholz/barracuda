//! NFC card reader on an MFRC522-compatible chip, such as the M5Stack Dial's
//! WS1850S.
//!
//! Initialization soft-resets the chip and switches the antenna on, as
//! M5Stack's M5Dial library does (`MFRC522::PCD_Init`); the chip needs no
//! reset or interrupt line. A poll runs ISO/IEC 14443-A activation through
//! `barracuda-driver-iso14443a`; a block read then sends the Type 2 `READ` to
//! the selected card.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_iso14443a as iso14443a;
use barracuda_driver_mfrc522::{Error as ChipError, Mfrc522};
use barracuda_peripheral::{
    PeripheralImplementation,
    nfc_reader::{BLOCK_BYTES, Card, NfcReader, Uid},
};
use embassy_time::Delay;
use embedded_hal::i2c::I2c;

/// Board-owned reader address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mfrc522NfcReaderConfig {
    address: u8,
}

impl Mfrc522NfcReaderConfig {
    /// Creates a configuration for the reader at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Reader failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Mfrc522NfcReaderError<E> {
    /// The chip or its bus failed.
    Reader(ChipError<E>),
    /// No card is selected: poll first, or the card has left the field.
    NoCard,
    /// The card did not answer a read, as a MIFARE Classic card does without
    /// authentication.
    NoAnswer,
    /// Several cards answered at once.
    Collision,
    /// A card answered outside the protocol.
    Protocol,
}

impl<E> From<iso14443a::Error<ChipError<E>>> for Mfrc522NfcReaderError<E> {
    fn from(error: iso14443a::Error<ChipError<E>>) -> Self {
        match error {
            iso14443a::Error::Reader(ChipError::Collision) | iso14443a::Error::Collision => {
                Self::Collision
            }
            iso14443a::Error::Reader(error) => Self::Reader(error),
            iso14443a::Error::NoAnswer => Self::NoAnswer,
            iso14443a::Error::Protocol => Self::Protocol,
        }
    }
}

/// Initialized card reader.
pub struct Mfrc522NfcReader<I2C> {
    chip: Mfrc522<I2C, Delay>,
    selected: bool,
}

/// Static factory used by generated Board composition.
pub struct Mfrc522NfcReaderImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Mfrc522NfcReaderImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = Mfrc522NfcReaderConfig;
    type Peripheral = Mfrc522NfcReader<I2C>;
    type Error = ChipError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let mut chip = Mfrc522::new(i2c, Delay, config.address)?;
        chip.initialize().await?;
        Ok(Mfrc522NfcReader {
            chip,
            selected: false,
        })
    }
}

impl<I2C: I2c> NfcReader for Mfrc522NfcReader<I2C> {
    type Error = Mfrc522NfcReaderError<I2C::Error>;

    async fn poll(&mut self) -> Result<Option<Card>, Self::Error> {
        self.selected = false;
        let activated = match iso14443a::activate(&mut self.chip).await {
            Ok(activated) => activated,
            // The card left the field during activation.
            Err(iso14443a::Error::NoAnswer) => None,
            Err(error) => return Err(error.into()),
        };
        let Some(activated) = activated else {
            return Ok(None);
        };
        let uid = Uid::new(activated.uid()).ok_or(Mfrc522NfcReaderError::Protocol)?;
        self.selected = true;
        Ok(Some(Card::new(uid, activated.atqa(), activated.sak())))
    }

    async fn read_block(&mut self, block: u8) -> Result<[u8; BLOCK_BYTES], Self::Error> {
        if !self.selected {
            return Err(Mfrc522NfcReaderError::NoCard);
        }
        let result = iso14443a::read(&mut self.chip, block).await;
        // A card that refuses a read falls back to idle and must be polled
        // again.
        self.selected = result.is_ok();
        Ok(result?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects, clippy::panic)]

    extern crate std;

    use core::convert::Infallible;
    use std::{collections::VecDeque, vec::Vec};

    use barracuda_driver_iso14443a::crc_a;
    use barracuda_peripheral::{PeripheralImplementation, nfc_reader::NfcReader};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Mfrc522NfcReaderConfig, Mfrc522NfcReaderError, Mfrc522NfcReaderImplementation};

    /// A WS1850S that answers each transceive from a script.
    struct Chip {
        registers: [u8; 0x40],
        fifo: VecDeque<u8>,
        answers: VecDeque<Option<Vec<u8>>>,
    }

    impl Chip {
        fn new(answers: Vec<Option<Vec<u8>>>) -> Self {
            let mut registers = [0; 0x40];
            registers[0x37] = 0x15;
            Self {
                registers,
                fifo: VecDeque::new(),
                answers: answers.into(),
            }
        }
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x28);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([0x09, data @ ..]) => self.fifo.extend(data.iter()),
                    Operation::Write([0x0a, value]) if *value & 0x80 != 0 => self.fifo.clear(),
                    Operation::Write([0x04, 0x7f]) => self.registers[0x04] = 0,
                    Operation::Write([0x0d, value]) if *value & 0x80 != 0 => {
                        self.fifo.clear();
                        match self.answers.pop_front().flatten() {
                            Some(answer) => {
                                self.fifo.extend(answer);
                                self.registers[0x04] = 0x30;
                            }
                            None => self.registers[0x04] = 0x01,
                        }
                    }
                    Operation::Write([reg, value]) => self.registers[usize::from(*reg)] = *value,
                    Operation::Write(_) => {}
                    Operation::Read(bytes) => {
                        for byte in bytes.iter_mut() {
                            *byte = match register {
                                0x09 => self.fifo.pop_front().expect("FIFO byte"),
                                0x0a => self.fifo.len() as u8,
                                // A soft reset completes at once.
                                0x01 => 0,
                                _ => self.registers[usize::from(register)],
                            };
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn with_crc(bytes: &[u8]) -> Vec<u8> {
        let mut frame = bytes.to_vec();
        frame.extend(crc_a(bytes));
        frame
    }

    #[test]
    fn polls_a_card_and_reads_it() {
        let uid = [0x04, 0x01, 0x02, 0x03];
        let bcc = uid.iter().fold(0, |bcc, byte| bcc ^ byte);
        let page: Vec<u8> = (0..16).map(|byte| byte * 3).collect();
        let chip = Chip::new(std::vec![
            None,
            None,
            Some(std::vec![0x44, 0x00]),
            Some(std::vec![uid[0], uid[1], uid[2], uid[3], bcc]),
            Some(with_crc(&[0x00])),
            Some(with_crc(&page)),
        ]);
        let mut reader = block_on(Mfrc522NfcReaderImplementation::initialize(
            chip,
            Mfrc522NfcReaderConfig::new(0x28),
        ))
        .expect("initialize");
        // An empty field: REQA and WUPA both time out.
        assert_eq!(block_on(reader.poll()), Ok(None));
        assert_eq!(
            block_on(reader.read_block(0)),
            Err(Mfrc522NfcReaderError::NoCard)
        );
        let card = block_on(reader.poll()).expect("poll").expect("a card");
        assert_eq!(card.uid().as_bytes(), uid);
        assert_eq!(card.atqa(), 0x0044);
        assert_eq!(card.sak(), 0x00);
        assert_eq!(block_on(reader.read_block(4)).expect("read").to_vec(), page);
    }
}
