//! I2C protocol driver for the Microchip ATECC608 secure element.
//!
//! The protocol follows Microchip's ATECC608 data sheet and CryptoAuthLib
//! (`calib_command.c`, `calib_execution.c`, `atca_helpers.c`):
//!
//! - **Wake.** The chip sleeps until SDA stays low for t<sub>WLO</sub>
//!   (60 µs). A write of one zero byte to address 00h holds it low for nine
//!   bit times, enough at 100 kHz and too short at 400 kHz, which is why
//!   CryptoAuthLib drops its bus to 100 kHz to wake the chip. After
//!   t<sub>WHI</sub> (1.5 ms) the chip answers `04 11 33 43`.
//! - **Packets.** The host writes word address 03h, then a count byte, the
//!   opcode, Param1, Param2 (little-endian), data, and a CRC-16 of the bytes
//!   from count to data. The chip answers with a count byte, data, and the
//!   same CRC. A four-byte answer carries a status byte; status 00h is
//!   success for commands that return no data.
//! - **Execution.** While a command runs, the chip does not acknowledge
//!   its address; the host polls until it does. Word address 01h puts the
//!   chip to sleep, which also clears its volatile TempKey.
//!
//! The driver performs single transactions; waiting is the caller's.

#![no_std]

use embedded_hal::i2c::{ErrorKind, I2c};

/// What a freshly woken chip answers: count 4, status 11h, and the CRC.
pub const WAKE_RESPONSE: [u8; 4] = [0x04, 0x11, 0x33, 0x43];
/// Longest answer this driver reads: count, 64 data bytes, and the CRC.
pub const MAX_RESPONSE: usize = 67;
/// Longest command data this driver sends (a Nonce pass-through).
pub const MAX_COMMAND_DATA: usize = 32;
/// Bytes of the factory serial number.
pub const SERIAL_NUMBER_LENGTH: usize = 9;

const WORD_SLEEP: u8 = 0x01;
const WORD_IDLE: u8 = 0x02;
const WORD_COMMAND: u8 = 0x03;
/// Count, opcode, Param1, two Param2 bytes, and two CRC bytes.
const COMMAND_OVERHEAD: usize = 7;
const CRC_POLYNOMIAL: u16 = 0x8005;

/// A command opcode this driver sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    /// Reads a zone of the chip's memory.
    Read,
    /// Loads or generates TempKey.
    Nonce,
    /// Generates random bytes.
    Random,
    /// Reports the chip's revision and state.
    Info,
    /// Generates a key, or computes the public key of a stored one.
    GenKey,
    /// Signs with a stored private key.
    Sign,
}

impl Opcode {
    /// Returns the opcode byte.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Read => 0x02,
            Self::Nonce => 0x16,
            Self::Random => 0x1b,
            Self::Info => 0x30,
            Self::GenKey => 0x40,
            Self::Sign => 0x41,
        }
    }

    /// Returns the longest the command runs, in milliseconds: CryptoAuthLib's
    /// ATECC608 execution times at the slowest clock divider (M2), with
    /// margin. A caller polls up to this long.
    #[must_use]
    pub const fn max_execution_ms(self) -> u32 {
        match self {
            Self::Read | Self::Info => 10,
            Self::Nonce => 40,
            Self::Random => 50,
            Self::GenKey | Self::Sign => 300,
        }
    }
}

/// One command packet's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command<'a> {
    opcode: Opcode,
    param1: u8,
    param2: u16,
    data: &'a [u8],
}

impl<'a> Command<'a> {
    /// Info mode 00h: the four-byte revision; byte 2 is 60h on the ATECC608.
    #[must_use]
    pub const fn info_revision() -> Self {
        Self::new(Opcode::Info, 0x00, 0x0000, &[])
    }

    /// Reads 32-byte block `block` of the configuration zone (Read mode
    /// 80h: 32 bytes, configuration zone).
    #[must_use]
    pub const fn read_config_block(block: u8) -> Self {
        // Param2 counts 32-byte blocks from bit 3 for a 32-byte read.
        Self::new(Opcode::Read, 0x80, (block as u16) << 3, &[])
    }

    /// Random mode 00h: 32 random bytes; the 20-byte input is required
    /// and ignored in this mode.
    #[must_use]
    pub const fn random(input: &'a [u8; 20]) -> Self {
        Self::new(Opcode::Random, 0x00, 0x0000, input)
    }

    /// GenKey mode 00h: the public key of the private key in `slot`.
    #[must_use]
    pub const fn public_key(slot: u8) -> Self {
        Self::new(Opcode::GenKey, 0x00, slot as u16, &[])
    }

    /// Nonce mode 03h: write `digest` to TempKey unchanged (pass-through).
    #[must_use]
    pub const fn load_digest(digest: &'a [u8; 32]) -> Self {
        Self::new(Opcode::Nonce, 0x03, 0x0000, digest)
    }

    /// Sign mode 80h: sign the external message held in TempKey with the
    /// private key in `slot`.
    #[must_use]
    pub const fn sign_tempkey(slot: u8) -> Self {
        Self::new(Opcode::Sign, 0x80, slot as u16, &[])
    }

    const fn new(opcode: Opcode, param1: u8, param2: u16, data: &'a [u8]) -> Self {
        Self {
            opcode,
            param1,
            param2,
            data,
        }
    }

    /// Returns the command's opcode.
    #[must_use]
    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }
}

/// A non-success status byte from the chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// 01h: a CheckMac or Verify miscompare.
    Miscompare,
    /// 03h: the command was malformed or not allowed.
    Parse,
    /// 05h: an ECC computation failed; the command may be retried.
    EccFault,
    /// 07h: a self test failed.
    SelfTest,
    /// 08h: the random generator's health test failed.
    HealthTest,
    /// 0Fh: the command could not execute, for example on an empty slot.
    Execution,
    /// 11h: the chip has just woken.
    AfterWake,
    /// EEh: the watchdog will put the chip to sleep before the command ends.
    WatchdogAboutToExpire,
    /// FFh: the chip received a corrupted packet.
    Communication,
    /// Any other status byte.
    Other(u8),
}

impl Status {
    const fn from_byte(byte: u8) -> Self {
        match byte {
            0x01 => Self::Miscompare,
            0x03 => Self::Parse,
            0x05 => Self::EccFault,
            0x07 => Self::SelfTest,
            0x08 => Self::HealthTest,
            0x0f => Self::Execution,
            0x11 => Self::AfterWake,
            0xee => Self::WatchdogAboutToExpire,
            0xff => Self::Communication,
            other => Self::Other(other),
        }
    }
}

/// Transport or protocol failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The chip did not acknowledge: it is executing a command or asleep.
    NotAcknowledged,
    /// The wake answer was not `04 11 33 43`.
    UnexpectedWake([u8; 4]),
    /// The answer's count byte is outside the possible range.
    InvalidLength(u8),
    /// The answer's CRC does not match.
    Crc,
    /// The chip answered with a failure status.
    Status(Status),
    /// The command's data exceeds [`MAX_COMMAND_DATA`].
    CommandTooLong,
}

/// An ATECC608 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Atecc608 {
    address: u8,
}

impl Atecc608 {
    /// Creates a driver when `address` is a valid seven-bit I2C address
    /// other than the general-call address the wake pulse uses.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address == 0 || address > 0x7f {
            None
        } else {
            Some(Self { address })
        }
    }

    /// Sends the wake pulse: one zero byte to address 00h. Nothing
    /// acknowledges it, so its result is ignored; read the wake answer
    /// t<sub>WHI</sub> later with [`Self::read_wake_response`].
    pub fn wake<I2C: I2c>(&self, i2c: &mut I2C) {
        let _ = i2c.write(0x00, &[0x00]);
    }

    /// Reads the answer of a freshly woken chip.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedWake`] when the chip was not asleep or
    /// answered otherwise, and the bus errors of [`Self::receive`].
    pub fn read_wake_response<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Error<I2C::Error>> {
        let mut answer = [0u8; 4];
        i2c.read(self.address, &mut answer).map_err(bus_error)?;
        if answer == WAKE_RESPONSE {
            Ok(())
        } else {
            Err(Error::UnexpectedWake(answer))
        }
    }

    /// Puts the chip to sleep; it forgets TempKey.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn sleep<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Error<I2C::Error>> {
        i2c.write(self.address, &[WORD_SLEEP]).map_err(bus_error)
    }

    /// Puts the chip into idle; it keeps TempKey and stops its watchdog.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn idle<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Error<I2C::Error>> {
        i2c.write(self.address, &[WORD_IDLE]).map_err(bus_error)
    }

    /// Sends one command packet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommandTooLong`] before any transfer, and the bus
    /// error when the write fails.
    pub fn send<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        command: &Command<'_>,
    ) -> Result<(), Error<I2C::Error>> {
        let mut packet = [0u8; 1 + COMMAND_OVERHEAD + MAX_COMMAND_DATA];
        let length = encode_command(command, &mut packet)?;
        let packet = packet.get(..length).ok_or(Error::CommandTooLong)?;
        i2c.write(self.address, packet).map_err(bus_error)
    }

    /// Reads one answer into `buffer` and returns its data, without the
    /// count and CRC. A status answer of 00h returns no data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAcknowledged`] while the chip still executes,
    /// [`Error::Status`] for a failure status, and [`Error::InvalidLength`]
    /// or [`Error::Crc`] for a corrupted answer.
    pub fn receive<'b, I2C: I2c>(
        &self,
        i2c: &mut I2C,
        buffer: &'b mut [u8; MAX_RESPONSE],
    ) -> Result<&'b [u8], Error<I2C::Error>> {
        let (count, rest) = buffer.split_at_mut(1);
        i2c.read(self.address, count).map_err(bus_error)?;
        let length = usize::from(count[0]);
        if !(4..=MAX_RESPONSE).contains(&length) {
            return Err(Error::InvalidLength(count[0]));
        }
        let tail = rest
            .get_mut(..length.saturating_sub(1))
            .ok_or(Error::InvalidLength(count[0]))?;
        i2c.read(self.address, tail).map_err(bus_error)?;
        decode_response(buffer.get(..length).ok_or(Error::InvalidLength(0))?)
    }
}

/// Writes word address, count, header, data, and CRC into `packet`;
/// returns the bytes used.
fn encode_command<E>(command: &Command<'_>, packet: &mut [u8]) -> Result<usize, Error<E>> {
    if command.data.len() > MAX_COMMAND_DATA {
        return Err(Error::CommandTooLong);
    }
    let count = COMMAND_OVERHEAD.saturating_add(command.data.len());
    let total = count.saturating_add(1);
    let packet = packet.get_mut(..total).ok_or(Error::CommandTooLong)?;
    let [param2_low, param2_high] = command.param2.to_le_bytes();
    packet[0] = WORD_COMMAND;
    packet[1] = u8::try_from(count).map_err(|_| Error::CommandTooLong)?;
    packet[2] = command.opcode.code();
    packet[3] = command.param1;
    packet[4] = param2_low;
    packet[5] = param2_high;
    let crc_at = total.saturating_sub(2);
    packet
        .get_mut(6..crc_at)
        .ok_or(Error::CommandTooLong)?
        .copy_from_slice(command.data);
    let crc = crc16(packet.get(1..crc_at).ok_or(Error::CommandTooLong)?);
    packet
        .get_mut(crc_at..total)
        .ok_or(Error::CommandTooLong)?
        .copy_from_slice(&crc.to_le_bytes());
    Ok(total)
}

/// Checks an answer's CRC and status; returns its data.
fn decode_response<E>(answer: &[u8]) -> Result<&[u8], Error<E>> {
    let length = answer.len();
    let crc_at = length.saturating_sub(2);
    let (body, crc) = answer.split_at(crc_at);
    if crc != crc16(body).to_le_bytes() {
        return Err(Error::Crc);
    }
    let data = body.get(1..).unwrap_or_default();
    if length == 4 {
        return match data.first().copied().unwrap_or(0xff) {
            0x00 => Ok(&[]),
            status => Err(Error::Status(Status::from_byte(status))),
        };
    }
    Ok(data)
}

/// The chip's CRC-16: polynomial 8005h, initial value zero, each byte's
/// bits taken least significant first, sent little-endian
/// (CryptoAuthLib `atCRC`).
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        for bit in 0..8 {
            let data_bit = (byte >> bit) & 1;
            let crc_bit = u8::from(crc & 0x8000 != 0);
            crc <<= 1;
            if data_bit != crc_bit {
                crc ^= CRC_POLYNOMIAL;
            }
        }
    }
    crc
}

/// Extracts the nine-byte serial number from configuration block 0: bytes
/// 0 to 3 and 8 to 12.
#[must_use]
pub fn serial_number(config_block_0: &[u8]) -> Option<[u8; SERIAL_NUMBER_LENGTH]> {
    let high = config_block_0.get(0..4)?;
    let low = config_block_0.get(8..13)?;
    let mut serial = [0u8; SERIAL_NUMBER_LENGTH];
    serial[..4].copy_from_slice(high);
    serial[4..].copy_from_slice(low);
    Some(serial)
}

/// Returns whether 32 random bytes are the fixed pattern an ATECC608 with
/// an unlocked configuration zone returns instead of random data.
#[must_use]
pub fn is_unlocked_random_pattern(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .chunks(4)
            .all(|chunk| chunk == [0xff, 0xff, 0x00, 0x00])
}

fn bus_error<E: embedded_hal::i2c::Error>(error: E) -> Error<E> {
    if matches!(error.kind(), ErrorKind::NoAcknowledge(_)) {
        Error::NotAcknowledged
    } else {
        Error::Bus(error)
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

    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    use super::{
        Atecc608, Command, Error, MAX_RESPONSE, Status, WAKE_RESPONSE, crc16,
        is_unlocked_random_pattern, serial_number,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> ErrorKind {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        }
    }

    /// Records writes and plays queued answers back through reads.
    #[derive(Default)]
    struct Chip {
        writes: Vec<(u8, Vec<u8>)>,
        answer: VecDeque<u8>,
        busy_reads: u8,
    }

    impl Chip {
        fn answer(&mut self, data: &[u8]) {
            let mut packet = std::vec![u8::try_from(data.len() + 3).expect("short")];
            packet.extend_from_slice(data);
            let crc = crc16(&packet);
            packet.extend_from_slice(&crc.to_le_bytes());
            self.answer.extend(packet);
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
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.writes.push((address, bytes.to_vec()));
                        if address != 0x35 {
                            return Err(Nack);
                        }
                    }
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

    #[test]
    fn crc_matches_the_wake_answer() {
        assert_eq!(crc16(&WAKE_RESPONSE[..2]).to_le_bytes(), [0x33, 0x43]);
    }

    #[test]
    fn wakes_with_a_zero_byte_to_the_general_call_address() {
        let mut chip = Chip::default();
        chip.answer.extend(WAKE_RESPONSE);
        let element = Atecc608::new(0x35).expect("address");
        element.wake(&mut chip);
        element.read_wake_response(&mut chip).expect("awake");
        assert_eq!(chip.writes, [(0x00, std::vec![0x00])]);
    }

    #[test]
    fn an_awake_chip_reports_an_unexpected_wake() {
        let mut chip = Chip::default();
        chip.answer.extend([0x04, 0x00, 0x03, 0x40]);
        let element = Atecc608::new(0x35).expect("address");
        assert_eq!(
            element.read_wake_response(&mut chip),
            Err(Error::UnexpectedWake([0x04, 0x00, 0x03, 0x40]))
        );
    }

    #[test]
    fn encodes_the_info_command_as_cryptoauthlib_does() {
        let mut chip = Chip::default();
        let element = Atecc608::new(0x35).expect("address");
        element
            .send(&mut chip, &Command::info_revision())
            .expect("send");
        // CryptoAuthLib's Info revision packet: 03 07 30 00 00 00 03 5D.
        assert_eq!(
            chip.writes,
            [(
                0x35,
                std::vec![0x03, 0x07, 0x30, 0x00, 0x00, 0x00, 0x03, 0x5d]
            )]
        );
    }

    #[test]
    fn encodes_parameters_little_endian_and_appends_data() {
        let mut chip = Chip::default();
        let element = Atecc608::new(0x35).expect("address");
        element
            .send(&mut chip, &Command::read_config_block(1))
            .expect("send");
        let digest = [0xab; 32];
        element
            .send(&mut chip, &Command::load_digest(&digest))
            .expect("send");
        let (_, read) = &chip.writes[0];
        assert_eq!(&read[..6], &[0x03, 0x07, 0x02, 0x80, 0x08, 0x00]);
        let (_, nonce) = &chip.writes[1];
        assert_eq!(nonce.len(), 1 + 7 + 32);
        assert_eq!(&nonce[..6], &[0x03, 39, 0x16, 0x03, 0x00, 0x00]);
        assert_eq!(&nonce[6..38], &digest);
        assert_eq!(nonce[38..].to_vec(), crc16(&nonce[1..38]).to_le_bytes());
    }

    #[test]
    fn receives_data_and_checks_its_crc() {
        let mut chip = Chip::default();
        chip.answer(&[0x00, 0x00, 0x60, 0x03]);
        let element = Atecc608::new(0x35).expect("address");
        let mut buffer = [0u8; MAX_RESPONSE];
        let data = element.receive(&mut chip, &mut buffer).expect("answer");
        assert_eq!(data, [0x00, 0x00, 0x60, 0x03]);

        chip.answer
            .extend([0x07, 0x00, 0x00, 0x60, 0x03, 0x00, 0x00]);
        assert_eq!(element.receive(&mut chip, &mut buffer), Err(Error::Crc));
    }

    #[test]
    fn maps_status_answers() {
        let mut chip = Chip::default();
        let element = Atecc608::new(0x35).expect("address");
        let mut buffer = [0u8; MAX_RESPONSE];
        chip.answer(&[0x00]);
        assert_eq!(element.receive(&mut chip, &mut buffer), Ok(&[][..]));
        chip.answer(&[0x0f]);
        assert_eq!(
            element.receive(&mut chip, &mut buffer),
            Err(Error::Status(Status::Execution))
        );
    }

    #[test]
    fn a_busy_chip_is_not_acknowledged() {
        let mut chip = Chip {
            busy_reads: 1,
            ..Chip::default()
        };
        chip.answer(&[0x00]);
        let element = Atecc608::new(0x35).expect("address");
        let mut buffer = [0u8; MAX_RESPONSE];
        assert_eq!(
            element.receive(&mut chip, &mut buffer),
            Err(Error::NotAcknowledged)
        );
        assert_eq!(element.receive(&mut chip, &mut buffer), Ok(&[][..]));
    }

    #[test]
    fn extracts_the_serial_number() {
        let block: Vec<u8> = (0..32).collect();
        assert_eq!(serial_number(&block), Some([0, 1, 2, 3, 8, 9, 10, 11, 12]));
    }

    #[test]
    fn recognizes_the_unlocked_random_pattern() {
        let pattern: Vec<u8> = [0xff, 0xff, 0x00, 0x00].repeat(8);
        assert!(is_unlocked_random_pattern(&pattern));
        let mut random = pattern.clone();
        random[5] = 0x42;
        assert!(!is_unlocked_random_pattern(&random));
    }
}
