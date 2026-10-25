//! NFC card reader on an ST25R3916, such as the M5Stack PaperMono's and
//! StackChan's.
//!
//! Where the Board names an M5IOE1 output that switches the reader's supply
//! (the PaperMono's G4), initialization drives it high and waits, as
//! M5Stack's UIFlow `NFC` class does, before initializing the reader for
//! ISO/IEC 14443-A. A poll runs activation through
//! `barracuda-driver-iso14443a`; a block read then sends the Type 2 `READ` to
//! the selected card.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_iso14443a as iso14443a;
use barracuda_driver_m5ioe1::{Error as ExpanderError, M5ioe1};
use barracuda_driver_st25r3916::{Error as ChipError, St25r3916};
use barracuda_peripheral::{
    PeripheralImplementation,
    nfc_reader::{BLOCK_BYTES, Card, NfcReader, Uid},
};
use embassy_time::{Delay, Timer};
use embedded_hal::i2c::I2c;

/// Board-owned reader address and supply switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct St25r3916NfcReaderConfig {
    address: u8,
    power_pin: Option<u8>,
    expander_address: u8,
    settle_ms: u32,
}

impl St25r3916NfcReaderConfig {
    /// Creates a configuration: the reader's address, the M5IOE1 output
    /// (`IO1` to `IO14`) that powers it if any, the expander's address, and
    /// how long the supply settles.
    #[must_use]
    pub fn new(address: u8, power_pin: Option<u8>, expander_address: u8, settle_ms: i64) -> Self {
        Self {
            address,
            power_pin,
            expander_address,
            settle_ms: u32::try_from(settle_ms).unwrap_or(0),
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum St25r3916InitError<E> {
    /// The supply switch on the M5IOE1 failed.
    Expander(ExpanderError<E>),
    /// The reader failed to start.
    Reader(ChipError<E>),
}

impl<E> From<ExpanderError<E>> for St25r3916InitError<E> {
    fn from(error: ExpanderError<E>) -> Self {
        Self::Expander(error)
    }
}

impl<E> From<ChipError<E>> for St25r3916InitError<E> {
    fn from(error: ChipError<E>) -> Self {
        Self::Reader(error)
    }
}

/// Reader failure.
#[derive(Debug, PartialEq, Eq)]
pub enum St25r3916NfcReaderError<E> {
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

impl<E> From<iso14443a::Error<ChipError<E>>> for St25r3916NfcReaderError<E> {
    fn from(error: iso14443a::Error<ChipError<E>>) -> Self {
        match error {
            iso14443a::Error::Reader(error) => Self::Reader(error),
            iso14443a::Error::NoAnswer => Self::NoAnswer,
            iso14443a::Error::Collision => Self::Collision,
            iso14443a::Error::Protocol => Self::Protocol,
        }
    }
}

/// Initialized card reader.
pub struct St25r3916NfcReader<I2C> {
    chip: St25r3916<I2C, Delay>,
    selected: bool,
}

/// Static factory used by generated Board composition.
pub struct St25r3916NfcReaderImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for St25r3916NfcReaderImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = St25r3916NfcReaderConfig;
    type Peripheral = St25r3916NfcReader<I2C>;
    type Error = St25r3916InitError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = i2c;
        if let Some(pin) = config.power_pin {
            let mut expander = M5ioe1::new(&mut i2c, config.expander_address)?;
            expander.wake()?;
            expander.configure_output(pin, true)?;
        }
        if config.settle_ms > 0 {
            Timer::after_millis(u64::from(config.settle_ms)).await;
        }
        let mut chip = St25r3916::new(i2c, Delay, config.address)?;
        chip.initialize().await?;
        Ok(St25r3916NfcReader {
            chip,
            selected: false,
        })
    }
}

impl<I2C: I2c> NfcReader for St25r3916NfcReader<I2C> {
    type Error = St25r3916NfcReaderError<I2C::Error>;

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
        let uid = Uid::new(activated.uid()).ok_or(St25r3916NfcReaderError::Protocol)?;
        self.selected = true;
        Ok(Some(Card::new(uid, activated.atqa(), activated.sak())))
    }

    async fn read_block(&mut self, block: u8) -> Result<[u8; BLOCK_BYTES], Self::Error> {
        if !self.selected {
            return Err(St25r3916NfcReaderError::NoCard);
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
    use std::vec::Vec;

    use barracuda_peripheral::{PeripheralImplementation, nfc_reader::NfcReader};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        St25r3916NfcReaderConfig, St25r3916NfcReaderError, St25r3916NfcReaderImplementation,
    };

    /// The PaperMono's internal bus: the M5IOE1 at 4Fh and an ST25R3916 at
    /// 50h with no card in its field.
    struct Bus {
        expander: [u8; 0x30],
        expander_writes: Vec<(u8, u8)>,
        reader: [u8; 0x40],
        reader_seen_before_power: bool,
    }

    impl Default for Bus {
        fn default() -> Self {
            Self {
                expander: [0; 0x30],
                expander_writes: Vec::new(),
                reader: [0; 0x40],
                reader_seen_before_power: false,
            }
        }
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut pointer = 0;
            for operation in operations {
                match (address, operation) {
                    (0x4f, Operation::Write([register])) => pointer = *register,
                    (0x4f, Operation::Write([register, value])) => {
                        self.expander[usize::from(*register)] = *value;
                        self.expander_writes.push((*register, *value));
                    }
                    (0x4f, Operation::Read(bytes)) => {
                        for (offset, byte) in bytes.iter_mut().enumerate() {
                            *byte = self.expander[usize::from(pointer) + offset];
                        }
                    }
                    (0x50, operation) => {
                        if self.expander[0x05] & 0x08 == 0 {
                            self.reader_seen_before_power = true;
                        }
                        match operation {
                            Operation::Write([register]) => pointer = *register,
                            Operation::Write([register, value]) if *register < 0x40 => {
                                self.reader[usize::from(*register)] = *value;
                            }
                            Operation::Write(_) => {}
                            Operation::Read(bytes) => {
                                let register = pointer & 0x3f;
                                bytes.fill(match register {
                                    // IC identity: ST25R3916, revision 2.
                                    0x3f => 0x2a,
                                    // Oscillator running at once.
                                    0x1a => 0x80,
                                    0x31 => 0x10,
                                    // An empty FIFO: no card answers.
                                    0x1e | 0x1f => 0x00,
                                    _ => self.reader[usize::from(register)],
                                });
                            }
                        }
                    }
                    _ => panic!("unexpected I2C address {address:#x}"),
                }
            }
            Ok(())
        }
    }

    #[test]
    fn powers_the_reader_from_the_m5ioe1_before_talking_to_it() {
        // PaperMono: supply on M5IOE1 G4 (UIFlow `hardware/nfc.py`).
        let mut reader = block_on(St25r3916NfcReaderImplementation::initialize(
            Bus::default(),
            St25r3916NfcReaderConfig::new(0x50, Some(4), 0x4f, 1),
        ))
        .expect("initialize");
        assert_eq!(block_on(reader.poll()), Ok(None));
        assert_eq!(
            block_on(reader.read_block(0)),
            Err(St25r3916NfcReaderError::NoCard)
        );
        let (bus, _) = reader.chip.release();
        assert!(!bus.reader_seen_before_power);
        // IO4: output latch high, push-pull, output mode.
        assert_eq!(bus.expander[0x05] & 0x08, 0x08);
        assert_eq!(bus.expander[0x03] & 0x08, 0x08);
        assert!(!bus.expander_writes.is_empty());
    }
}
