//! Low-level Driver for one NXP MFRC522-compatible 13.56 MHz reader on I2C,
//! such as the WS1850S of the M5Stack Dial.
//!
//! Register addresses and the initialization follow the MFRC522 datasheet
//! and M5Stack's port of the Arduino MFRC522 library in the M5Dial library
//! (`utility/MFRC522.cpp`): a soft reset, a 25 ms frame timer started at the
//! end of each transmission (`TModeReg = 80h`, `TPrescalerReg = A9h`, reload
//! 1000), 100 % ASK (`TxASKReg = 40h`), the CRC preset `6363h`
//! (`ModeReg = 3Dh`), and both antenna drivers on. On I2C the register
//! address is sent as is; reads write it and then read.
//!
//! The chip neither appends nor checks CRC_A here (`TxModeReg` and
//! `RxModeReg` keep their CRC bits clear): [`Transceiver::transceive_crc`]
//! computes and checks it in software, so the card's answer is verified the
//! same way on every reader.

#![no_std]

use barracuda_driver_iso14443a::{Transceiver, command, crc_a, crc_a_matches};
use embedded_hal::i2c::I2c;
use embedded_hal_async::delay::DelayNs;

/// I2C address of the M5Stack Dial's WS1850S (M5Dial `RC522_I2C_ADDRESS`).
pub const DEFAULT_ADDRESS: u8 = 0x28;

const COMMAND: u8 = 0x01;
const COM_IRQ: u8 = 0x04;
const ERROR: u8 = 0x06;
const FIFO_DATA: u8 = 0x09;
const FIFO_LEVEL: u8 = 0x0a;
const CONTROL: u8 = 0x0c;
const BIT_FRAMING: u8 = 0x0d;
const COLL: u8 = 0x0e;
const MODE: u8 = 0x11;
const TX_MODE: u8 = 0x12;
const RX_MODE: u8 = 0x13;
const TX_CONTROL: u8 = 0x14;
const TX_ASK: u8 = 0x15;
const MOD_WIDTH: u8 = 0x24;
const T_MODE: u8 = 0x2a;
const T_PRESCALER: u8 = 0x2b;
const T_RELOAD_HIGH: u8 = 0x2c;
const T_RELOAD_LOW: u8 = 0x2d;
const VERSION: u8 = 0x37;

const CMD_IDLE: u8 = 0x00;
const CMD_TRANSCEIVE: u8 = 0x0c;
const CMD_SOFT_RESET: u8 = 0x0f;

/// `CommandReg` bit 4: the analog part is powered down, as during reset.
const POWER_DOWN: u8 = 1 << 4;
/// `ComIrqReg`: receive done, command idle, frame timer expired.
const RX_IRQ: u8 = 1 << 5;
const IDLE_IRQ: u8 = 1 << 4;
const TIMER_IRQ: u8 = 1 << 0;
/// Writing this to `ComIrqReg` clears every request bit.
const CLEAR_IRQS: u8 = 0x7f;
/// `ErrorReg`: buffer overflow, parity, and protocol errors end a frame;
/// a collision is reported apart.
const FRAME_ERRORS: u8 = (1 << 4) | (1 << 1) | (1 << 0);
const COLLISION_ERROR: u8 = 1 << 3;
/// `FIFOLevelReg` bit 7 empties the FIFO.
const FLUSH_FIFO: u8 = 1 << 7;
/// `BitFramingReg` bit 7 starts a transceive's transmission.
const START_SEND: u8 = 1 << 7;
/// `CollReg` bit 7: keep received bits after a collision; cleared for
/// activation.
const VALUES_AFTER_COLL: u8 = 1 << 7;
/// `TxControlReg`: drive both antenna pins.
const ANTENNA_ON: u8 = 0x03;
/// The FIFO holds 64 bytes.
const FIFO_BYTES: usize = 64;

/// How long a reset may take before the chip leaves power-down
/// (the M5Dial library waits 50 ms, then polls).
const RESET_MS: u32 = 50;
const RESET_POLLS: u32 = 50;
/// Longest wait for one frame: past the chip's own 25 ms timer, as the
/// Arduino library's 36 ms deadline.
const FRAME_POLLS: u32 = 36;

/// MFRC522 register or bus failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The chip stayed in power-down after a soft reset.
    ResetTimeout,
    /// The chip answered with a version of `00h` or `FFh`.
    NotPresent(u8),
    /// A frame was too long for the FIFO.
    FrameTooLong,
    /// The chip neither finished nor timed out a frame.
    Stalled,
    /// The card's answer had a parity, protocol, or buffer error.
    Frame(u8),
    /// Several cards answered at once.
    Collision,
    /// The answer's CRC_A was wrong.
    Crc,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// One answer: the bytes received and the valid bits of the last one
/// (0 for a whole byte).
struct Answer {
    len: usize,
    last_bits: u8,
}

/// One MFRC522 with its delay source.
pub struct Mfrc522<I2C, D> {
    i2c: I2C,
    delay: D,
    address: u8,
}

impl<I2C, D> Mfrc522<I2C, D>
where
    I2C: I2c,
    D: DelayNs,
{
    /// Wraps a chip at `address`; nothing is sent until [`Self::initialize`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAddress`] for a non-seven-bit address.
    pub fn new(i2c: I2C, delay: D, address: u8) -> Result<Self, Error<I2C::Error>> {
        if address > 0x7f {
            return Err(Error::InvalidAddress);
        }
        Ok(Self {
            i2c,
            delay,
            address,
        })
    }

    /// Returns the bus and delay.
    pub fn release(self) -> (I2C, D) {
        (self.i2c, self.delay)
    }

    /// Resets the chip, sets up the frame timer, modulation, and CRC preset,
    /// and turns the antenna on. Returns the `VersionReg` value (`91h` or
    /// `92h` on an NXP MFRC522).
    ///
    /// # Errors
    ///
    /// Returns a bus error, [`Error::ResetTimeout`], or
    /// [`Error::NotPresent`].
    pub async fn initialize(&mut self) -> Result<u8, Error<I2C::Error>> {
        self.write(COMMAND, CMD_SOFT_RESET)?;
        self.delay.delay_ms(RESET_MS).await;
        let mut polls: u32 = 0;
        while self.read(COMMAND)? & POWER_DOWN != 0 {
            polls = polls.saturating_add(1);
            if polls > RESET_POLLS {
                return Err(Error::ResetTimeout);
            }
            self.delay.delay_ms(1).await;
        }
        let version = self.read(VERSION)?;
        if version == 0x00 || version == 0xff {
            return Err(Error::NotPresent(version));
        }
        for (register, value) in [
            (TX_MODE, 0x00),
            (RX_MODE, 0x00),
            (MOD_WIDTH, 0x26),
            (T_MODE, 0x80),
            (T_PRESCALER, 0xa9),
            (T_RELOAD_HIGH, 0x03),
            (T_RELOAD_LOW, 0xe8),
            (TX_ASK, 0x40),
            (MODE, 0x3d),
        ] {
            self.write(register, value)?;
        }
        let control = self.read(TX_CONTROL)?;
        if control & ANTENNA_ON != ANTENNA_ON {
            self.write(TX_CONTROL, control | ANTENNA_ON)?;
        }
        Ok(version)
    }

    /// Sends `frame`, its last byte holding `tx_last_bits` bits (0 for a
    /// whole byte), and copies the card's answer into `reply`; `None` when
    /// the frame timer expired without an answer.
    async fn transceive(
        &mut self,
        frame: &[u8],
        tx_last_bits: u8,
        reply: &mut [u8],
    ) -> Result<Option<Answer>, Error<I2C::Error>> {
        if frame.len() > FIFO_BYTES {
            return Err(Error::FrameTooLong);
        }
        self.write(COMMAND, CMD_IDLE)?;
        self.write(COM_IRQ, CLEAR_IRQS)?;
        self.write(FIFO_LEVEL, FLUSH_FIFO)?;
        self.write_fifo(frame)?;
        self.write(BIT_FRAMING, tx_last_bits & 0x07)?;
        self.write(COMMAND, CMD_TRANSCEIVE)?;
        self.write(BIT_FRAMING, (tx_last_bits & 0x07) | START_SEND)?;
        let mut polls: u32 = 0;
        loop {
            let irqs = self.read(COM_IRQ)?;
            if irqs & (RX_IRQ | IDLE_IRQ) != 0 {
                break;
            }
            if irqs & TIMER_IRQ != 0 {
                return Ok(None);
            }
            polls = polls.saturating_add(1);
            if polls > FRAME_POLLS {
                return Err(Error::Stalled);
            }
            self.delay.delay_ms(1).await;
        }
        let errors = self.read(ERROR)?;
        if errors & FRAME_ERRORS != 0 {
            return Err(Error::Frame(errors));
        }
        if errors & COLLISION_ERROR != 0 {
            return Err(Error::Collision);
        }
        let available = usize::from(self.read(FIFO_LEVEL)? & 0x7f);
        let len = available.min(reply.len());
        let target = reply.get_mut(..len).ok_or(Error::FrameTooLong)?;
        self.read_fifo(target)?;
        let last_bits = self.read(CONTROL)? & 0x07;
        Ok(Some(Answer { len, last_bits }))
    }

    fn read(&mut self, register: u8) -> Result<u8, Error<I2C::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .map_err(Error::Bus)
    }

    fn write_fifo(&mut self, bytes: &[u8]) -> Result<(), Error<I2C::Error>> {
        let mut buffer = [0; FIFO_BYTES + 1];
        let end = bytes.len().checked_add(1).ok_or(Error::FrameTooLong)?;
        let target = buffer.get_mut(..end).ok_or(Error::FrameTooLong)?;
        target[0] = FIFO_DATA;
        target
            .get_mut(1..)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(bytes);
        self.i2c.write(self.address, target).map_err(Error::Bus)
    }

    fn read_fifo(&mut self, bytes: &mut [u8]) -> Result<(), Error<I2C::Error>> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.i2c
            .write_read(self.address, &[FIFO_DATA], bytes)
            .map_err(Error::Bus)
    }
}

impl<I2C, D> Transceiver for Mfrc522<I2C, D>
where
    I2C: I2c,
    D: DelayNs,
{
    type Error = Error<I2C::Error>;

    async fn request(&mut self, wake: bool) -> Result<Option<u16>, Self::Error> {
        // Activation needs the bits received before a collision cleared.
        let coll = self.read(COLL)?;
        self.write(COLL, coll & !VALUES_AFTER_COLL)?;
        let code = if wake { command::WUPA } else { command::REQA };
        let mut atqa = [0; 2];
        let answer = match self.transceive(&[code], 7, &mut atqa).await {
            Ok(answer) => answer,
            // Several cards answer REQA at once; their ATQA bits collide.
            Err(Error::Collision) => Some(Answer {
                len: 2,
                last_bits: 0,
            }),
            Err(error) => return Err(error),
        };
        Ok(match answer {
            Some(Answer {
                len: 2,
                last_bits: 0,
            }) => Some(u16::from_le_bytes(atqa)),
            _ => None,
        })
    }

    async fn anticollision(&mut self, sel: u8) -> Result<Option<[u8; 5]>, Self::Error> {
        let mut answer = [0; 5];
        Ok(
            match self
                .transceive(&[sel, command::NVB_ANTICOLLISION], 0, &mut answer)
                .await?
            {
                Some(Answer {
                    len: 5,
                    last_bits: 0,
                }) => Some(answer),
                _ => None,
            },
        )
    }

    async fn transceive_crc(
        &mut self,
        frame: &[u8],
        reply: &mut [u8],
    ) -> Result<Option<usize>, Self::Error> {
        let mut outgoing = [0; FIFO_BYTES];
        let crc_at = frame.len();
        let end = crc_at.checked_add(2).ok_or(Error::FrameTooLong)?;
        outgoing
            .get_mut(..crc_at)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(frame);
        outgoing
            .get_mut(crc_at..end)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(&crc_a(frame));
        let mut incoming = [0; FIFO_BYTES];
        let wanted = reply.len().checked_add(2).ok_or(Error::FrameTooLong)?;
        let incoming = incoming.get_mut(..wanted).ok_or(Error::FrameTooLong)?;
        let Some(answer) = self
            .transceive(outgoing.get(..end).ok_or(Error::FrameTooLong)?, 0, incoming)
            .await?
        else {
            return Ok(None);
        };
        let received = incoming.get(..answer.len).ok_or(Error::FrameTooLong)?;
        if answer.last_bits != 0 || !crc_a_matches(received) {
            // A four-bit NAK or a damaged answer.
            return if answer.len == 1 && answer.last_bits == 4 {
                Ok(Some(0))
            } else {
                Err(Error::Crc)
            };
        }
        let len = answer.len.saturating_sub(2);
        reply
            .get_mut(..len)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(received.get(..len).ok_or(Error::FrameTooLong)?);
        Ok(Some(len))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects, clippy::panic)]

    extern crate std;

    use core::convert::Infallible;
    use std::{collections::VecDeque, vec::Vec};

    use barracuda_driver_iso14443a::{activate, crc_a, read};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};
    use embedded_hal_async::delay::DelayNs;

    use super::{
        BIT_FRAMING, CMD_SOFT_RESET, CMD_TRANSCEIVE, COM_IRQ, COMMAND, ERROR, Error, FIFO_DATA,
        FIFO_LEVEL, Mfrc522, TX_CONTROL, VERSION,
    };

    const ADDRESS: u8 = 0x28;

    /// A register model of the chip with a card that answers each
    /// transceive from a script.
    struct Chip {
        registers: [u8; 0x40],
        fifo: VecDeque<u8>,
        sent: Vec<(Vec<u8>, u8)>,
        answers: VecDeque<Option<(Vec<u8>, u8)>>,
    }

    impl Chip {
        fn new(answers: Vec<Option<(Vec<u8>, u8)>>) -> Self {
            let mut registers = [0; 0x40];
            registers[usize::from(VERSION)] = 0x15;
            registers[usize::from(TX_CONTROL)] = 0x80;
            Self {
                registers,
                fifo: VecDeque::new(),
                sent: Vec::new(),
                answers: answers.into(),
            }
        }

        fn start_send(&mut self) {
            let frame: Vec<u8> = self.fifo.drain(..).collect();
            let bits = self.registers[usize::from(BIT_FRAMING)] & 0x07;
            self.sent.push((frame, bits));
            match self.answers.pop_front().flatten() {
                Some((answer, last_bits)) => {
                    self.fifo.extend(answer);
                    self.registers[usize::from(super::CONTROL)] = last_bits;
                    self.registers[usize::from(COM_IRQ)] = 0x30;
                }
                None => self.registers[usize::from(COM_IRQ)] = 0x01,
            }
        }

        fn write(&mut self, register: u8, value: u8) {
            match register {
                FIFO_DATA => self.fifo.push_back(value),
                FIFO_LEVEL if value & 0x80 != 0 => self.fifo.clear(),
                COM_IRQ if value == 0x7f => self.registers[usize::from(COM_IRQ)] = 0,
                // A soft reset leaves power-down at once in this model.
                COMMAND if value == CMD_SOFT_RESET => self.registers[usize::from(COMMAND)] = 0,
                BIT_FRAMING if value & 0x80 != 0 => {
                    assert_eq!(self.registers[usize::from(COMMAND)], CMD_TRANSCEIVE);
                    self.registers[usize::from(BIT_FRAMING)] = value;
                    self.start_send();
                }
                _ => self.registers[usize::from(register)] = value,
            }
        }

        fn read(&mut self, register: u8) -> u8 {
            match register {
                FIFO_DATA => self.fifo.pop_front().expect("FIFO byte"),
                FIFO_LEVEL => self.fifo.len() as u8,
                _ => self.registers[usize::from(register)],
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
            assert_eq!(address, ADDRESS);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, values @ ..]) => {
                        for value in values.iter() {
                            self.write(*reg, *value);
                        }
                    }
                    Operation::Read(bytes) => {
                        for byte in bytes.iter_mut() {
                            *byte = self.read(register);
                        }
                    }
                    Operation::Write([]) => {}
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay;

    impl DelayNs for Delay {
        async fn delay_ns(&mut self, _ns: u32) {}
    }

    fn with_crc(bytes: &[u8]) -> Vec<u8> {
        let mut frame = bytes.to_vec();
        frame.extend(crc_a(bytes));
        frame
    }

    #[test]
    fn initializes_the_timer_modulation_and_antenna() {
        let mut reader = Mfrc522::new(Chip::new(Vec::new()), Delay, ADDRESS).expect("address");
        assert_eq!(block_on(reader.initialize()), Ok(0x15));
        let (chip, _) = reader.release();
        assert_eq!(chip.registers[0x2a], 0x80);
        assert_eq!(chip.registers[0x2b], 0xa9);
        assert_eq!(chip.registers[0x2c], 0x03);
        assert_eq!(chip.registers[0x2d], 0xe8);
        assert_eq!(chip.registers[0x15], 0x40);
        assert_eq!(chip.registers[0x11], 0x3d);
        assert_eq!(chip.registers[usize::from(TX_CONTROL)], 0x83);
    }

    #[test]
    fn refuses_an_absent_chip() {
        let mut chip = Chip::new(Vec::new());
        chip.registers[usize::from(VERSION)] = 0xff;
        let mut reader = Mfrc522::new(chip, Delay, ADDRESS).expect("address");
        assert_eq!(block_on(reader.initialize()), Err(Error::NotPresent(0xff)));
        assert!(matches!(
            Mfrc522::new(Chip::new(Vec::new()), Delay, 0x80),
            Err(Error::InvalidAddress)
        ));
    }

    #[test]
    fn activates_a_card_and_reads_a_page() {
        let uid = [0x04, 0xa1, 0xb2, 0xc3];
        let bcc = uid[0] ^ uid[1] ^ uid[2] ^ uid[3];
        let page: Vec<u8> = (0..16).collect();
        let chip = Chip::new(std::vec![
            Some((std::vec![0x44, 0x00], 0)),
            Some((std::vec![uid[0], uid[1], uid[2], uid[3], bcc], 0)),
            Some((with_crc(&[0x00]), 0)),
            Some((with_crc(&page), 0)),
        ]);
        let mut reader = Mfrc522::new(chip, Delay, ADDRESS).expect("address");
        block_on(reader.initialize()).expect("initialize");
        let card = block_on(activate(&mut reader))
            .expect("activate")
            .expect("a card");
        assert_eq!(card.uid(), uid);
        assert_eq!(card.atqa(), 0x0044);
        assert_eq!(card.sak(), 0x00);
        let data = block_on(read(&mut reader, 4)).expect("read");
        assert_eq!(data.to_vec(), page);
        let (chip, _) = reader.release();
        // REQA goes out as seven bits; the rest as whole bytes, SELECT and
        // READ with their CRC_A.
        assert_eq!(chip.sent[0], (std::vec![0x26], 7));
        assert_eq!(chip.sent[1], (std::vec![0x93, 0x20], 0));
        assert_eq!(
            chip.sent[2].0,
            with_crc(&[0x93, 0x70, uid[0], uid[1], uid[2], uid[3], bcc])
        );
        assert_eq!(chip.sent[3].0, [0x30, 0x04, 0x26, 0xee]);
    }

    #[test]
    fn an_empty_field_times_out_into_no_card() {
        let chip = Chip::new(std::vec![None, None]);
        let mut reader = Mfrc522::new(chip, Delay, ADDRESS).expect("address");
        block_on(reader.initialize()).expect("initialize");
        assert_eq!(block_on(activate(&mut reader)), Ok(None));
        let (chip, _) = reader.release();
        assert_eq!(chip.sent.len(), 2, "REQA then WUPA");
        assert_eq!(chip.sent[1].0, [0x52]);
    }

    #[test]
    fn a_damaged_answer_fails_its_crc() {
        let chip = Chip::new(std::vec![Some((std::vec![0x00, 0x12, 0x34], 0))]);
        let mut reader = Mfrc522::new(chip, Delay, ADDRESS).expect("address");
        let mut reply = [0; 1];
        assert_eq!(
            block_on(barracuda_driver_iso14443a::Transceiver::transceive_crc(
                &mut reader,
                &[0x30, 0x00],
                &mut reply
            )),
            Err(Error::Crc)
        );
        // A frame error the chip flags ends the transfer as one.
        let mut chip = Chip::new(std::vec![Some((std::vec![0x00], 0))]);
        chip.registers[usize::from(ERROR)] = 0x02;
        let mut reader = Mfrc522::new(chip, Delay, ADDRESS).expect("address");
        assert_eq!(
            block_on(barracuda_driver_iso14443a::Transceiver::transceive_crc(
                &mut reader,
                &[0x30, 0x00],
                &mut reply
            )),
            Err(Error::Frame(0x02))
        );
    }
}
