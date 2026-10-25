//! Microchip ATECC608 secure-element implementation.
//!
//! Every operation wakes the chip, runs its commands, and puts the chip back
//! to sleep, as CryptoAuthLib does, so the chip's 1.3 s watchdog never
//! expires mid-operation and the chip draws its sleep current between
//! operations. Commands are polled: after sending one, the implementation
//! reads once a millisecond until the chip acknowledges, up to the
//! command's longest execution time.
//!
//! Start-up wakes the chip and reads its revision (Info mode 00h), which
//! must name the ATECC608 family (byte 2 = 60h), so a missing or different
//! chip fails the Board at boot.
//!
//! - The serial number is configuration bytes 0 to 3 and 8 to 12 (Read).
//! - Random bytes come from Random mode 00h, 32 at a time; a chip whose
//!   configuration zone is unlocked returns a fixed pattern instead, which
//!   is reported as [`Atecc608Error::Unlocked`].
//! - A public key is GenKey mode 00h on the slot.
//! - A signature loads the digest into TempKey (Nonce pass-through, mode
//!   03h) and signs it (Sign mode 80h), within one wake, since sleep clears
//!   TempKey.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_atecc608::{
    Atecc608, Command, Error as ChipError, MAX_RESPONSE, Opcode, SERIAL_NUMBER_LENGTH,
    is_unlocked_random_pattern, serial_number,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    secure_element::{KeySlot, P256PublicKey, P256Signature, SecureElement, SerialNumber},
};
use embassy_time::Timer;
use embedded_hal::i2c::I2c;

/// The chip's key slots.
pub const SLOT_COUNT: u8 = 16;
/// t<sub>WHI</sub>, 1.5 ms, with margin.
const WAKE_DELAY_US: u64 = 1_600;
/// Info revision byte 2 of the ATECC608A and ATECC608B.
const ATECC608_FAMILY: u8 = 0x60;

/// Board-owned secure-element address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Atecc608Config {
    address: u8,
}

impl Atecc608Config {
    /// Creates configuration for the chip at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Secure-element failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Atecc608Error<E> {
    /// The address is not a usable seven-bit address.
    InvalidAddress(u8),
    /// A transfer, packet, or chip status failed.
    Chip(ChipError<E>),
    /// The chip did not finish a command in time.
    Timeout(Opcode),
    /// The chip did not wake.
    NoWake,
    /// The revision does not name an ATECC608.
    UnexpectedDevice([u8; 4]),
    /// An answer had an unexpected length.
    UnexpectedLength(usize),
    /// The configuration zone is unlocked, so the chip returns no random
    /// data.
    Unlocked,
    /// The slot does not exist.
    InvalidSlot(u8),
}

impl<E> From<ChipError<E>> for Atecc608Error<E> {
    fn from(error: ChipError<E>) -> Self {
        Self::Chip(error)
    }
}

/// Initialized secure element.
pub struct Atecc608SecureElement<I2C> {
    i2c: I2C,
    chip: Atecc608,
    revision: [u8; 4],
}

impl<I2C> Atecc608SecureElement<I2C> {
    /// Returns the revision the chip reported at start-up.
    #[must_use]
    pub const fn revision(&self) -> [u8; 4] {
        self.revision
    }
}

/// Static factory used by generated Board composition.
pub struct Atecc608SecureElementImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Atecc608SecureElementImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = Atecc608Config;
    type Peripheral = Atecc608SecureElement<I2C>;
    type Error = Atecc608Error<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let chip =
            Atecc608::new(config.address).ok_or(Atecc608Error::InvalidAddress(config.address))?;
        let mut element = Atecc608SecureElement {
            i2c,
            chip,
            revision: [0; 4],
        };
        element.wake().await?;
        let result = element.execute::<4>(&Command::info_revision()).await;
        element.sleep();
        let revision = result?;
        if revision[2] != ATECC608_FAMILY {
            return Err(Atecc608Error::UnexpectedDevice(revision));
        }
        element.revision = revision;
        Ok(element)
    }
}

impl<I2C: I2c> Atecc608SecureElement<I2C> {
    /// Wakes the chip. A chip still awake from a cancelled operation does
    /// not answer the pulse as a woken one; it is put to sleep and woken
    /// again.
    async fn wake(&mut self) -> Result<(), Atecc608Error<I2C::Error>> {
        for _ in 0..2 {
            self.chip.wake(&mut self.i2c);
            Timer::after_micros(WAKE_DELAY_US).await;
            match self.chip.read_wake_response(&mut self.i2c) {
                Ok(()) => return Ok(()),
                Err(ChipError::UnexpectedWake(_) | ChipError::NotAcknowledged) => {
                    let _ = self.chip.sleep(&mut self.i2c);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(Atecc608Error::NoWake)
    }

    /// Puts the chip to sleep; a failure leaves it to its watchdog.
    fn sleep(&mut self) {
        let _ = self.chip.sleep(&mut self.i2c);
    }

    /// Sends `command` and polls for an answer of exactly `N` data bytes.
    async fn execute<const N: usize>(
        &mut self,
        command: &Command<'_>,
    ) -> Result<[u8; N], Atecc608Error<I2C::Error>> {
        self.chip.send(&mut self.i2c, command)?;
        let mut buffer = [0u8; MAX_RESPONSE];
        for _ in 0..command.opcode().max_execution_ms() {
            Timer::after_millis(1).await;
            match self.chip.receive(&mut self.i2c, &mut buffer) {
                Ok(data) => {
                    return data
                        .try_into()
                        .map_err(|_| Atecc608Error::UnexpectedLength(data.len()));
                }
                Err(ChipError::NotAcknowledged) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Atecc608Error::Timeout(command.opcode()))
    }

    fn check_slot(slot: KeySlot) -> Result<u8, Atecc608Error<I2C::Error>> {
        if slot.index() < SLOT_COUNT {
            Ok(slot.index())
        } else {
            Err(Atecc608Error::InvalidSlot(slot.index()))
        }
    }
}

impl<I2C: I2c> SecureElement for Atecc608SecureElement<I2C> {
    type Error = Atecc608Error<I2C::Error>;

    async fn serial_number(&mut self) -> Result<SerialNumber, Self::Error> {
        self.wake().await?;
        let result = self.execute::<32>(&Command::read_config_block(0)).await;
        self.sleep();
        let block = result?;
        let serial: [u8; SERIAL_NUMBER_LENGTH] =
            serial_number(&block).ok_or(Atecc608Error::UnexpectedLength(block.len()))?;
        SerialNumber::new(&serial).ok_or(Atecc608Error::UnexpectedLength(serial.len()))
    }

    async fn fill_random(&mut self, buffer: &mut [u8]) -> Result<(), Self::Error> {
        let input = [0u8; 20];
        for chunk in buffer.chunks_mut(32) {
            self.wake().await?;
            let result = self.execute::<32>(&Command::random(&input)).await;
            self.sleep();
            let random = result?;
            if is_unlocked_random_pattern(&random) {
                return Err(Atecc608Error::Unlocked);
            }
            if let Some(source) = random.get(..chunk.len()) {
                chunk.copy_from_slice(source);
            }
        }
        Ok(())
    }

    async fn public_key(&mut self, slot: KeySlot) -> Result<P256PublicKey, Self::Error> {
        let slot = Self::check_slot(slot)?;
        self.wake().await?;
        let result = self.execute::<64>(&Command::public_key(slot)).await;
        self.sleep();
        result.map(P256PublicKey::new)
    }

    async fn sign_digest(
        &mut self,
        slot: KeySlot,
        digest: &[u8; 32],
    ) -> Result<P256Signature, Self::Error> {
        let slot = Self::check_slot(slot)?;
        self.wake().await?;
        let result = match self.execute::<0>(&Command::load_digest(digest)).await {
            Ok([]) => self.execute::<64>(&Command::sign_tempkey(slot)).await,
            Err(error) => Err(error),
        };
        self.sleep();
        result.map(P256Signature::new)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod tests {
    extern crate std;

    use std::{collections::VecDeque, vec::Vec};

    use barracuda_driver_atecc608::{WAKE_RESPONSE, crc16};
    use barracuda_peripheral::{
        PeripheralImplementation,
        secure_element::{KeySlot, SecureElement},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    use super::{Atecc608Config, Atecc608Error, Atecc608SecureElementImplementation};

    #[derive(Debug, PartialEq, Eq)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> ErrorKind {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        }
    }

    /// An ATECC608B model at 35h: sleeps until woken, executes Info, Read,
    /// Random, GenKey, Nonce, and Sign, and stays busy for two reads after
    /// each command.
    struct Chip {
        awake: bool,
        answer: VecDeque<u8>,
        busy_reads: u8,
        tempkey: Option<[u8; 32]>,
        locked: bool,
        log: Vec<u8>,
    }

    impl Chip {
        fn new(locked: bool) -> Self {
            Self {
                awake: false,
                answer: VecDeque::new(),
                busy_reads: 0,
                tempkey: None,
                locked,
                log: Vec::new(),
            }
        }

        fn reply(&mut self, data: &[u8]) {
            let mut packet = std::vec![u8::try_from(data.len() + 3).expect("short")];
            packet.extend_from_slice(data);
            let crc = crc16(&packet);
            packet.extend_from_slice(&crc.to_le_bytes());
            self.answer = packet.into();
            self.busy_reads = 2;
        }

        fn command(&mut self, packet: &[u8]) {
            let count = usize::from(packet[0]);
            assert_eq!(packet.len(), count);
            assert_eq!(
                packet[count - 2..].to_vec(),
                crc16(&packet[..count - 2]).to_le_bytes()
            );
            let (opcode, mode, param2) = (
                packet[1],
                packet[2],
                u16::from_le_bytes([packet[3], packet[4]]),
            );
            let data = &packet[5..count - 2];
            self.log.push(opcode);
            match (opcode, mode) {
                (0x30, 0x00) => self.reply(&[0x00, 0x00, 0x60, 0x03]),
                (0x02, 0x80) => {
                    assert_eq!(param2, 0);
                    let block: Vec<u8> = (0..32).map(|i| 0x10 + i).collect();
                    self.reply(&block);
                }
                (0x1b, 0x00) => {
                    assert_eq!(data, [0; 20]);
                    let random: Vec<u8> = if self.locked {
                        (0..32).map(|i| 0xa0 ^ i).collect()
                    } else {
                        [0xff, 0xff, 0x00, 0x00].repeat(8)
                    };
                    self.reply(&random);
                }
                (0x40, 0x00) => self.reply(&[u8::try_from(param2).expect("slot"); 64]),
                (0x16, 0x03) => {
                    self.tempkey = Some(data.try_into().expect("32 bytes"));
                    self.reply(&[0x00]);
                }
                (0x41, 0x80) => match self.tempkey {
                    Some(digest) => {
                        let mut signature = [0u8; 64];
                        signature[..32].copy_from_slice(&digest);
                        signature[32..].fill(u8::try_from(param2).expect("slot"));
                        self.reply(&signature);
                    }
                    None => self.reply(&[0x0f]),
                },
                _ => self.reply(&[0x03]),
            }
        }
    }

    impl ErrorType for Chip {
        type Error = Nack;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address == 0x00 {
                if !self.awake {
                    self.awake = true;
                    self.answer = WAKE_RESPONSE.into();
                }
                return Err(Nack);
            }
            if address != 0x35 || !self.awake {
                return Err(Nack);
            }
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => match bytes[0] {
                        0x01 => {
                            self.awake = false;
                            self.tempkey = None;
                        }
                        0x03 => self.command(&bytes[1..]),
                        _ => {}
                    },
                    Operation::Read(buffer) => {
                        if self.busy_reads > 0 {
                            self.busy_reads -= 1;
                            return Err(Nack);
                        }
                        for value in buffer.iter_mut() {
                            *value = self.answer.pop_front().ok_or(Nack)?;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn start(chip: Chip) -> super::Atecc608SecureElement<Chip> {
        block_on(Atecc608SecureElementImplementation::initialize(
            chip,
            Atecc608Config::new(0x35),
        ))
        .expect("start")
    }

    #[test]
    fn starts_by_reading_the_revision_and_sleeps() {
        let element = start(Chip::new(true));
        assert_eq!(element.revision(), [0x00, 0x00, 0x60, 0x03]);
        assert!(!element.i2c.awake);
    }

    #[test]
    fn a_missing_chip_fails_at_start() {
        let result = block_on(Atecc608SecureElementImplementation::initialize(
            Chip::new(true),
            Atecc608Config::new(0x60),
        ));
        assert!(matches!(result, Err(Atecc608Error::NoWake)));
    }

    #[test]
    fn reads_the_serial_number() {
        let mut element = start(Chip::new(true));
        let serial = block_on(element.serial_number()).expect("serial");
        assert_eq!(
            serial.as_bytes(),
            [0x10, 0x11, 0x12, 0x13, 0x18, 0x19, 0x1a, 0x1b, 0x1c]
        );
    }

    #[test]
    fn fills_random_bytes_32_at_a_time() {
        let mut element = start(Chip::new(true));
        let mut buffer = [0u8; 40];
        block_on(element.fill_random(&mut buffer)).expect("random");
        assert_eq!(buffer[0], 0xa0);
        assert_eq!(buffer[33], 0xa1);
        assert_eq!(element.i2c.log, [0x30, 0x1b, 0x1b]);
    }

    #[test]
    fn an_unlocked_chip_returns_no_random_data() {
        let mut element = start(Chip::new(false));
        let mut buffer = [0u8; 8];
        assert_eq!(
            block_on(element.fill_random(&mut buffer)),
            Err(Atecc608Error::Unlocked)
        );
    }

    #[test]
    fn signs_a_digest_through_tempkey_in_one_wake() {
        let mut element = start(Chip::new(true));
        let digest = [0x5a; 32];
        let signature = block_on(element.sign_digest(KeySlot::new(0), &digest)).expect("signature");
        assert_eq!(&signature.as_bytes()[..32], &digest);
        assert_eq!(&signature.as_bytes()[32..], &[0; 32]);
        assert_eq!(element.i2c.log, [0x30, 0x16, 0x41]);
        let key = block_on(element.public_key(KeySlot::new(2))).expect("key");
        assert_eq!(key.as_bytes(), &[2; 64]);
        assert_eq!(
            block_on(element.public_key(KeySlot::new(16))),
            Err(Atecc608Error::InvalidSlot(16))
        );
    }

    #[test]
    fn a_chip_left_awake_is_put_to_sleep_and_woken_again() {
        let mut element = start(Chip::new(true));
        element.i2c.awake = true;
        element.i2c.answer.clear();
        let serial = block_on(element.serial_number()).expect("serial");
        assert_eq!(serial.as_bytes().len(), 9);
    }
}
