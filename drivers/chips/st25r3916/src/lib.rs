//! Low-level Driver for one ST25R3916 NFC reader on I2C, in ISO/IEC 14443-A
//! polling mode.
//!
//! Register addresses, direct commands, and the initialization follow
//! M5Stack's MIT-licensed ST25R3916 driver for its PaperMono, StackChan, and
//! NFC Unit (`uiflow-micropython`, `m5stack/libs/driver/st25r3916.py`), which
//! mirrors ST's RFAL: check the IC identity (type 5), restore the defaults,
//! select 3.3 V I2C at 400 kHz, set up the AM modulation depth and the
//! external-field thresholds, start the oscillator and wait for it, adjust
//! the regulators, then configure ISO/IEC 14443-A at 106 kbit/s and switch
//! the field on.
//!
//! On I2C a register read writes `40h | address` and reads; a write sends the
//! address and the value; space-B registers carry the `FBh` prefix; the FIFO
//! is loaded with `80h` and read with `9Fh`; a direct command is one byte.
//! Every wait polls the FIFO status, so the chip's interrupt line is not
//! needed.

#![no_std]

use barracuda_driver_iso14443a::{Transceiver, crc_a_matches};
use embedded_hal::i2c::I2c;
use embedded_hal_async::delay::DelayNs;

/// I2C address of the ST25R3916 (M5Stack PaperMono and StackChan pin maps).
pub const DEFAULT_ADDRESS: u8 = 0x50;

const OP_READ: u8 = 0x40;
const SPACE_B: u8 = 0xfb;
const LOAD_FIFO: u8 = 0x80;
const READ_FIFO: u8 = 0x9f;

const CMD_SET_DEFAULT: u8 = 0xc1;
const CMD_STOP_ALL_ACTIVITIES: u8 = 0xc2;
const CMD_TRANSMIT_WITH_CRC: u8 = 0xc4;
const CMD_TRANSMIT_WITHOUT_CRC: u8 = 0xc5;
const CMD_TRANSMIT_REQA: u8 = 0xc6;
const CMD_TRANSMIT_WUPA: u8 = 0xc7;
const CMD_NFC_INITIAL_FIELD_ON: u8 = 0xc8;
const CMD_RESET_RX_GAIN: u8 = 0xd5;
const CMD_ADJUST_REGULATORS: u8 = 0xd6;
const CMD_CLEAR_FIFO: u8 = 0xdb;
const CMD_TEST_ACCESS: u8 = 0xfc;

const IO_CONFIGURATION_1: u8 = 0x00;
const IO_CONFIGURATION_2: u8 = 0x01;
const OPERATION_CONTROL: u8 = 0x02;
const MODE_DEFINITION: u8 = 0x03;
const BITRATE_DEFINITION: u8 = 0x04;
const ISO14443A_SETTINGS: u8 = 0x05;
const PASSIVE_TARGET_DEFINITION: u8 = 0x08;
const AUXILIARY_DEFINITION: u8 = 0x0a;
const RECEIVER_CONFIGURATION_1: u8 = 0x0b;
const NO_RESPONSE_TIMER_1: u8 = 0x10;
const TIMER_AND_EMV_CONTROL: u8 = 0x12;
const MASK_MAIN_INTERRUPT: u8 = 0x16;
const MAIN_INTERRUPT: u8 = 0x1a;
const FIFO_STATUS_1: u8 = 0x1e;
const NUMBER_OF_TRANSMITTED_BYTES_1: u8 = 0x22;
const ANTENNA_TUNING_CONTROL_1: u8 = 0x26;
const ANTENNA_TUNING_CONTROL_2: u8 = 0x27;
const TX_DRIVER: u8 = 0x28;
const PASSIVE_TARGET_MODULATION: u8 = 0x29;
const FIELD_ACTIVATION_THRESHOLD: u8 = 0x2a;
const FIELD_DEACTIVATION_THRESHOLD: u8 = 0x2b;
const AUXILIARY_DISPLAY: u8 = 0x31;
const IC_IDENTITY: u8 = 0x3f;

const SPACE_B_EMD_SUPPRESSION: u8 = 0x05;
const SPACE_B_CORRELATOR_1: u8 = 0x0c;
const SPACE_B_CORRELATOR_2: u8 = 0x0d;
const SPACE_B_RESISTIVE_AM: u8 = 0x2a;
const SPACE_B_OVERSHOOT_1: u8 = 0x30;
const SPACE_B_OVERSHOOT_2: u8 = 0x31;
const SPACE_B_UNDERSHOOT_1: u8 = 0x32;
const SPACE_B_UNDERSHOOT_2: u8 = 0x33;

/// IC identity type of the ST25R3916 (bits 7:3).
const IDENTITY_TYPE: u8 = 0x05;
/// IO configuration: 3.3 V supply, I2C at 400 kHz.
const IO_CONFIGURATION_I2C_400K_3V3: [u8; 2] = [0x10, 0x84];
/// AM modulation depth nibble of the transmitter driver register.
const TX_AM_DEFAULT_NIBBLE: u8 = 13;
/// Operation control: oscillator, field detector, wake-up, receiver,
/// transmitter enables.
const OP_EN: u8 = 0x80;
const RX_EN: u8 = 0x40;
const TX_EN: u8 = 0x08;
const WU: u8 = 0x04;
const EN_FD: u8 = 0x03;
/// Main interrupt: oscillator stable.
const I_OSC: u8 = 0x80;
/// Auxiliary display: oscillator running.
const OSC_OK: u8 = 0x10;
/// IO configuration 2: antenna tuning.
const AAT_EN: u8 = 0x20;
/// Auxiliary definition: no CRC check on receive; disable correlator.
const NO_CRC_RX: u8 = 0x80;
const DIS_CORR: u8 = 0x04;
/// ISO 14443-A settings: anticollision framing.
const ANTCL: u8 = 0x01;
/// Interrupt masks before the regulators are adjusted (`MASK_PRE_ADJUST`).
const MASK_PRE_ADJUST: [u8; 4] = [0xff, 0xff, 0x00, 0xff];

/// Frame waiting times of M5Stack's driver.
const TIMEOUT_REQ_WUP_MS: u32 = 12;
const TIMEOUT_ANTICOLL_MS: u32 = 8;
const TIMEOUT_SELECT_MS: u32 = 8;
const TIMEOUT_READ_MS: u32 = 24;
/// The oscillator starts within 25 ms.
const OSCILLATOR_MS: u32 = 25;
/// The NFC carrier frequency, which the no-response timer counts in.
const FC_HZ: u64 = 13_560_000;
/// The FIFO holds 512 bytes; a frame of this driver is far shorter.
const MAX_FRAME_BYTES: usize = 32;

/// ST25R3916 register or bus failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The IC identity register does not name an ST25R3916.
    NotPresent(u8),
    /// The oscillator did not start.
    Oscillator,
    /// A frame is longer than this driver sends or receives.
    FrameTooLong,
    /// The answer's CRC_A was wrong.
    Crc,
    /// An I2C operation failed.
    Bus(BusError),
}

/// One ST25R3916 with its delay source.
pub struct St25r3916<I2C, D> {
    i2c: I2C,
    delay: D,
    address: u8,
}

impl<I2C, D> St25r3916<I2C, D>
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

    /// Initializes the chip for ISO/IEC 14443-A polling and switches the
    /// field on. Returns the IC identity register.
    ///
    /// # Errors
    ///
    /// Returns a bus error, [`Error::NotPresent`], or [`Error::Oscillator`].
    pub async fn initialize(&mut self) -> Result<u8, Error<I2C::Error>> {
        let identity = self.read(IC_IDENTITY)?;
        if identity == 0xff || identity >> 3 != IDENTITY_TYPE || identity & 0x07 == 0 {
            return Err(Error::NotPresent(identity));
        }
        self.command(CMD_SET_DEFAULT, &[]).await?;
        self.delay.delay_ms(10).await;
        self.command(CMD_TEST_ACCESS, &[0x04, 0x10]).await?;
        self.write(IO_CONFIGURATION_1, IO_CONFIGURATION_I2C_400K_3V3[0])?;
        self.write(IO_CONFIGURATION_2, IO_CONFIGURATION_I2C_400K_3V3[1])?;
        self.write(TX_DRIVER, TX_AM_DEFAULT_NIBBLE << 4)?;
        let io = self.read(IO_CONFIGURATION_1)?;
        self.write(IO_CONFIGURATION_1, io | 0x07)?;
        self.write_space_b(SPACE_B_RESISTIVE_AM, 0x80).await?;
        self.set_bits(IO_CONFIGURATION_2, AAT_EN)?;
        self.write_space_b(SPACE_B_RESISTIVE_AM, 0x00).await?;
        self.write(FIELD_ACTIVATION_THRESHOLD, 0x10 | 0x03)?;
        self.write(FIELD_DEACTIVATION_THRESHOLD, 0x02)?;
        let target = self.read(PASSIVE_TARGET_DEFINITION)?;
        self.write(PASSIVE_TARGET_DEFINITION, (target & 0x0f) | (0x05 << 4))?;
        self.write(PASSIVE_TARGET_MODULATION, 0x5f)?;
        self.write_space_b(SPACE_B_EMD_SUPPRESSION, 0x40).await?;
        self.write(ANTENNA_TUNING_CONTROL_1, 0x82)?;
        self.write(ANTENNA_TUNING_CONTROL_2, 0x82)?;
        self.set_bits(OPERATION_CONTROL, EN_FD)?;
        self.command(CMD_CLEAR_FIFO, &[]).await?;
        self.write_masks(MASK_PRE_ADJUST)?;
        self.clear_interrupts()?;
        self.enable_oscillator().await?;
        self.write_masks([0; 4])?;
        self.command(CMD_ADJUST_REGULATORS, &[]).await?;
        self.delay.delay_ms(5).await;
        self.configure_nfc_a().await?;
        Ok(identity)
    }

    async fn enable_oscillator(&mut self) -> Result<(), Error<I2C::Error>> {
        if self.read(OPERATION_CONTROL)? & OP_EN == 0 {
            let mask = self.read(MASK_MAIN_INTERRUPT)?;
            self.write(MASK_MAIN_INTERRUPT, mask & !I_OSC)?;
            self.clear_interrupts()?;
            self.set_bits(OPERATION_CONTROL, OP_EN)?;
            let mut started = false;
            for _ in 0..OSCILLATOR_MS {
                self.delay.delay_ms(1).await;
                if self.read(MAIN_INTERRUPT)? & I_OSC != 0 {
                    started = true;
                    break;
                }
            }
            self.set_bits(MASK_MAIN_INTERRUPT, I_OSC)?;
            if !started {
                return Err(Error::Oscillator);
            }
        }
        if self.read(AUXILIARY_DISPLAY)? & OSC_OK == 0 {
            return Err(Error::Oscillator);
        }
        Ok(())
    }

    async fn configure_nfc_a(&mut self) -> Result<(), Error<I2C::Error>> {
        self.command(CMD_STOP_ALL_ACTIVITIES, &[]).await?;
        self.clear_bits(OPERATION_CONTROL, WU)?;
        self.write(MODE_DEFINITION, 0x09)?;
        self.write(BITRATE_DEFINITION, 0x00)?;
        self.write(ISO14443A_SETTINGS, 0x00)?;
        self.clear_bits(AUXILIARY_DEFINITION, DIS_CORR)?;
        for (register, value) in [
            (SPACE_B_OVERSHOOT_1, 0x40),
            (SPACE_B_OVERSHOOT_2, 0x03),
            (SPACE_B_UNDERSHOOT_1, 0x40),
            (SPACE_B_UNDERSHOOT_2, 0x03),
            (SPACE_B_CORRELATOR_1, 0x47),
            (SPACE_B_CORRELATOR_2, 0x00),
        ] {
            self.write_space_b(register, value).await?;
        }
        // Receiver configuration 1-4: 600 kHz high-pass, stable gains.
        for (offset, value) in [0x08, 0x2d, 0xd8, 0x22].into_iter().enumerate() {
            let register = RECEIVER_CONFIGURATION_1.saturating_add(offset as u8);
            self.write(register, value)?;
        }
        self.write_masks([0; 4])?;
        self.command(CMD_RESET_RX_GAIN, &[]).await?;
        self.command(CMD_NFC_INITIAL_FIELD_ON, &[]).await?;
        self.delay.delay_ms(5).await;
        self.set_bits(OPERATION_CONTROL, TX_EN | RX_EN)
    }

    /// Prepares one exchange: frame waiting time, framing, CRC check, and an
    /// empty FIFO with no pending interrupt.
    async fn prepare(
        &mut self,
        timeout_ms: u32,
        settings: u8,
        check_crc: bool,
    ) -> Result<(), Error<I2C::Error>> {
        self.write_frame_waiting_time(timeout_ms)?;
        self.write(ISO14443A_SETTINGS, settings)?;
        if check_crc {
            self.clear_bits(AUXILIARY_DEFINITION, NO_CRC_RX)?;
        } else {
            self.set_bits(AUXILIARY_DEFINITION, NO_CRC_RX)?;
        }
        self.clear_interrupts()?;
        self.command(CMD_CLEAR_FIFO, &[]).await
    }

    /// Sends `frame` from the FIFO with `command`.
    async fn send(&mut self, frame: &[u8], command: u8) -> Result<(), Error<I2C::Error>> {
        self.load_fifo(frame).await?;
        let bytes = u16::try_from(frame.len()).map_err(|_| Error::FrameTooLong)?;
        let [high, low] = (bytes << 3).to_be_bytes();
        self.write(NUMBER_OF_TRANSMITTED_BYTES_1, high)?;
        self.write(NUMBER_OF_TRANSMITTED_BYTES_1.saturating_add(1), low)?;
        self.command(command, &[]).await
    }

    /// Waits until the FIFO holds at least `min_bytes` and then stops
    /// growing; returns its byte and last-byte bit counts, or zero bytes when
    /// the time ran out.
    async fn wait_answer(
        &mut self,
        min_bytes: usize,
        timeout_ms: u32,
    ) -> Result<(usize, u8), Error<I2C::Error>> {
        let mut last = None;
        for _ in 0..timeout_ms {
            let (bytes, bits) = self.fifo_size()?;
            if bytes >= min_bytes || (bytes == 0 && bits != 0) {
                if last == Some((bytes, bits)) {
                    return Ok((bytes, bits));
                }
                last = Some((bytes, bits));
            }
            self.delay.delay_ms(1).await;
        }
        Ok(last.unwrap_or((0, 0)))
    }

    fn fifo_size(&mut self) -> Result<(usize, u8), Error<I2C::Error>> {
        let status_1 = self.read(FIFO_STATUS_1)?;
        let status_2 = self.read(FIFO_STATUS_1.saturating_add(1))?;
        let bytes = usize::from(status_1) | (usize::from(status_2 & 0xc0) << 2);
        Ok((bytes, (status_2 >> 1) & 0x07))
    }

    fn write_frame_waiting_time(&mut self, timeout_ms: u32) -> Result<(), Error<I2C::Error>> {
        // The no-response timer counts 64/fc steps, or 4096/fc when
        // `nrt_step` is set.
        let step_cycles: u64 = if self.read(TIMER_AND_EMV_CONTROL)? & 0x01 != 0 {
            4096
        } else {
            64
        };
        let cycles = u64::from(timeout_ms).saturating_mul(FC_HZ).div_ceil(1000);
        let steps = cycles.div_ceil(step_cycles).clamp(1, 0xffff);
        let [high, low] = u16::try_from(steps).unwrap_or(u16::MAX).to_be_bytes();
        self.write(NO_RESPONSE_TIMER_1, high)?;
        self.write(NO_RESPONSE_TIMER_1.saturating_add(1), low)
    }

    fn write_masks(&mut self, masks: [u8; 4]) -> Result<(), Error<I2C::Error>> {
        for (offset, mask) in masks.into_iter().enumerate() {
            self.write(MASK_MAIN_INTERRUPT.saturating_add(offset as u8), mask)?;
        }
        Ok(())
    }

    /// Reads, and so clears, the four interrupt registers.
    fn clear_interrupts(&mut self) -> Result<(), Error<I2C::Error>> {
        for offset in 0..4 {
            self.read(MAIN_INTERRUPT.saturating_add(offset))?;
        }
        Ok(())
    }

    async fn command(&mut self, command: u8, payload: &[u8]) -> Result<(), Error<I2C::Error>> {
        let mut frame = [0; 3];
        let end = payload.len().checked_add(1).ok_or(Error::FrameTooLong)?;
        let target = frame.get_mut(..end).ok_or(Error::FrameTooLong)?;
        target[0] = command;
        target
            .get_mut(1..)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(payload);
        self.i2c.write(self.address, target).map_err(Error::Bus)?;
        // M5Stack's driver lets every direct command settle for 2 ms.
        self.delay.delay_ms(2).await;
        Ok(())
    }

    async fn load_fifo(&mut self, bytes: &[u8]) -> Result<(), Error<I2C::Error>> {
        let mut frame = [0; MAX_FRAME_BYTES + 1];
        let end = bytes.len().checked_add(1).ok_or(Error::FrameTooLong)?;
        let target = frame.get_mut(..end).ok_or(Error::FrameTooLong)?;
        target[0] = LOAD_FIFO;
        target
            .get_mut(1..)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(bytes);
        self.i2c.write(self.address, target).map_err(Error::Bus)?;
        self.delay.delay_us(50).await;
        Ok(())
    }

    fn read_fifo(&mut self, bytes: &mut [u8]) -> Result<(), Error<I2C::Error>> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.i2c
            .write_read(self.address, &[READ_FIFO], bytes)
            .map_err(Error::Bus)
    }

    fn read(&mut self, register: u8) -> Result<u8, Error<I2C::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register | OP_READ], &mut value)
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .map_err(Error::Bus)
    }

    async fn write_space_b(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[SPACE_B, register & 0x3f, value])
            .map_err(Error::Bus)?;
        self.delay.delay_us(50).await;
        Ok(())
    }

    fn set_bits(&mut self, register: u8, bits: u8) -> Result<(), Error<I2C::Error>> {
        let value = self.read(register)?;
        self.write(register, value | bits)
    }

    fn clear_bits(&mut self, register: u8, bits: u8) -> Result<(), Error<I2C::Error>> {
        let value = self.read(register)?;
        self.write(register, value & !bits)
    }
}

impl<I2C, D> Transceiver for St25r3916<I2C, D>
where
    I2C: I2c,
    D: DelayNs,
{
    type Error = Error<I2C::Error>;

    async fn request(&mut self, wake: bool) -> Result<Option<u16>, Self::Error> {
        self.prepare(TIMEOUT_REQ_WUP_MS, ANTCL, false).await?;
        let command = if wake {
            CMD_TRANSMIT_WUPA
        } else {
            CMD_TRANSMIT_REQA
        };
        self.command(command, &[]).await?;
        let (bytes, _) = self.wait_answer(2, TIMEOUT_REQ_WUP_MS).await?;
        if bytes < 2 {
            return Ok(None);
        }
        let mut atqa = [0; 2];
        self.read_fifo(&mut atqa)?;
        let atqa = u16::from_le_bytes(atqa);
        Ok((atqa != 0 && atqa != 0xffff).then_some(atqa))
    }

    async fn anticollision(&mut self, sel: u8) -> Result<Option<[u8; 5]>, Self::Error> {
        self.prepare(TIMEOUT_ANTICOLL_MS, ANTCL, true).await?;
        self.send(
            &[sel, barracuda_driver_iso14443a::command::NVB_ANTICOLLISION],
            CMD_TRANSMIT_WITHOUT_CRC,
        )
        .await?;
        let (bytes, _) = self.wait_answer(5, TIMEOUT_ANTICOLL_MS).await?;
        if bytes < 5 {
            return Ok(None);
        }
        let mut answer = [0; 5];
        self.read_fifo(&mut answer)?;
        Ok(Some(answer))
    }

    async fn transceive_crc(
        &mut self,
        frame: &[u8],
        reply: &mut [u8],
    ) -> Result<Option<usize>, Self::Error> {
        let timeout = if reply.len() > 1 {
            TIMEOUT_READ_MS
        } else {
            TIMEOUT_SELECT_MS
        };
        self.prepare(timeout, 0x00, true).await?;
        self.send(frame, CMD_TRANSMIT_WITH_CRC).await?;
        let (bytes, bits) = self.wait_answer(1, timeout).await?;
        if bytes == 0 {
            // Nothing, or a four-bit ACK or NAK.
            return Ok((bits != 0).then_some(0));
        }
        let mut answer = [0; MAX_FRAME_BYTES];
        let received = answer.get_mut(..bytes).ok_or(Error::FrameTooLong)?;
        self.read_fifo(received)?;
        // The chip leaves the checked CRC_A in the FIFO.
        let data = if bytes >= 3 && crc_a_matches(received) {
            received.get(..bytes.saturating_sub(2)).unwrap_or(&[])
        } else if bytes == reply.len() {
            &received[..]
        } else {
            return Err(Error::Crc);
        };
        let len = data.len().min(reply.len());
        reply
            .get_mut(..len)
            .ok_or(Error::FrameTooLong)?
            .copy_from_slice(data.get(..len).ok_or(Error::FrameTooLong)?);
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
        AUXILIARY_DISPLAY, Error, IC_IDENTITY, MAIN_INTERRUPT, NO_RESPONSE_TIMER_1,
        OPERATION_CONTROL, St25r3916,
    };

    const ADDRESS: u8 = 0x50;

    /// A register model: plain registers, a FIFO, direct commands logged, and
    /// a card that answers each transmission from a script.
    struct Chip {
        registers: [u8; 0x40],
        space_b: [u8; 0x40],
        fifo: VecDeque<u8>,
        commands: Vec<u8>,
        sent: Vec<Vec<u8>>,
        answers: VecDeque<Option<Vec<u8>>>,
    }

    impl Chip {
        fn new(answers: Vec<Option<Vec<u8>>>) -> Self {
            let mut registers = [0; 0x40];
            // ST25R3916 type 5, revision 2.
            registers[usize::from(IC_IDENTITY)] = (0x05 << 3) | 0x02;
            Self {
                registers,
                space_b: [0; 0x40],
                fifo: VecDeque::new(),
                commands: Vec::new(),
                sent: Vec::new(),
                answers: answers.into(),
            }
        }

        fn direct_command(&mut self, command: u8) {
            self.commands.push(command);
            match command {
                0xdb => self.fifo.clear(),
                0xc4..=0xc7 => {
                    let frame: Vec<u8> = match command {
                        0xc6 => std::vec![0x26],
                        0xc7 => std::vec![0x52],
                        _ => self.fifo.drain(..).collect(),
                    };
                    self.sent.push(frame);
                    if let Some(answer) = self.answers.pop_front().flatten() {
                        self.fifo.extend(answer);
                    }
                }
                _ => {}
            }
        }

        fn read(&mut self, register: u8) -> u8 {
            match register {
                // The oscillator starts at once in this model.
                MAIN_INTERRUPT => {
                    if self.registers[usize::from(OPERATION_CONTROL)] & 0x80 != 0 {
                        0x80
                    } else {
                        0
                    }
                }
                AUXILIARY_DISPLAY => 0x10,
                0x1e => self.fifo.len() as u8,
                0x1f => 0,
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
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write([byte]) if *byte >= 0xc0 && *byte != 0xfb => {
                        self.direct_command(*byte);
                    }
                    Operation::Write([byte]) => pointer = *byte,
                    Operation::Write([0xfb, register, value]) => {
                        self.space_b[usize::from(*register)] = *value;
                    }
                    Operation::Write([0x80, data @ ..]) => self.fifo.extend(data.iter()),
                    Operation::Write([0xfc, ..]) => self.commands.push(0xfc),
                    Operation::Write([register, value]) => {
                        self.registers[usize::from(*register)] = *value;
                    }
                    Operation::Write(_) => {}
                    Operation::Read(bytes) => {
                        if pointer == 0x9f {
                            for byte in bytes.iter_mut() {
                                *byte = self.fifo.pop_front().expect("FIFO byte");
                            }
                        } else {
                            assert_eq!(pointer & 0x40, 0x40, "register reads set bit 6");
                            bytes[0] = self.read(pointer & 0x3f);
                        }
                    }
                }
            }
            Ok(())
        }
    }

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
    fn initializes_like_m5stacks_driver_and_switches_the_field_on() {
        let mut reader = St25r3916::new(Chip::new(Vec::new()), Delay, ADDRESS).expect("address");
        assert_eq!(block_on(reader.initialize()), Ok(0x2a));
        let (chip, _) = reader.release();
        assert_eq!(&chip.commands[..2], [0xc1, 0xfc]);
        assert!(chip.commands.contains(&0xd6), "regulators adjusted");
        assert_eq!(
            chip.commands[chip.commands.len() - 1],
            0xc8,
            "field on last"
        );
        // 3.3 V I2C at 400 kHz, then the low bits of IO configuration 1 set.
        assert_eq!(chip.registers[0x00], 0x17);
        assert_eq!(chip.registers[0x01] & 0x20, 0x20, "antenna tuning on");
        assert_eq!(chip.registers[0x03], 0x09, "ISO 14443-A mode");
        assert_eq!(chip.registers[0x28], 0xd0);
        assert_eq!(
            chip.registers[usize::from(OPERATION_CONTROL)],
            0x80 | 0x40 | 0x08 | 0x03
        );
        assert_eq!(chip.space_b[0x2a], 0x00);
        assert_eq!(chip.space_b[0x05], 0x40);
        assert_eq!(chip.space_b[0x0c], 0x47);
    }

    #[test]
    fn refuses_another_chip() {
        let mut chip = Chip::new(Vec::new());
        chip.registers[usize::from(IC_IDENTITY)] = 0x8a;
        let mut reader = St25r3916::new(chip, Delay, ADDRESS).expect("address");
        assert_eq!(block_on(reader.initialize()), Err(Error::NotPresent(0x8a)));
    }

    #[test]
    fn activates_a_double_size_uid_and_reads_a_page() {
        let first = [0x88, 0x04, 0x11, 0x22];
        let second = [0x33, 0x44, 0x55, 0x66];
        let bcc = |part: [u8; 4]| part[0] ^ part[1] ^ part[2] ^ part[3];
        let page: Vec<u8> = (100..116).collect();
        let chip = Chip::new(std::vec![
            Some(std::vec![0x44, 0x00]),
            Some(std::vec![
                first[0],
                first[1],
                first[2],
                first[3],
                bcc(first)
            ]),
            Some(with_crc(&[0x04])),
            Some(std::vec![
                second[0],
                second[1],
                second[2],
                second[3],
                bcc(second)
            ]),
            Some(with_crc(&[0x00])),
            Some(with_crc(&page)),
        ]);
        let mut reader = St25r3916::new(chip, Delay, ADDRESS).expect("address");
        block_on(reader.initialize()).expect("initialize");
        let card = block_on(activate(&mut reader))
            .expect("activate")
            .expect("a card");
        assert_eq!(card.uid(), [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        assert_eq!(card.sak(), 0x00);
        let data = block_on(read(&mut reader, 8)).expect("read");
        assert_eq!(data.to_vec(), page);
        let (chip, _) = reader.release();
        assert_eq!(chip.sent[0], [0x26]);
        assert_eq!(chip.sent[1], [0x93, 0x20]);
        // The chip appends the CRC_A itself; the driver sends the bare frame.
        assert_eq!(
            chip.sent[2],
            [0x93, 0x70, 0x88, 0x04, 0x11, 0x22, bcc(first)]
        );
        assert_eq!(chip.sent[5], [0x30, 0x08]);
        // A 24 ms frame waiting time in 64/fc steps.
        let steps = u16::from_be_bytes([
            chip.registers[usize::from(NO_RESPONSE_TIMER_1)],
            chip.registers[usize::from(NO_RESPONSE_TIMER_1) + 1],
        ]);
        assert_eq!(steps, 5085);
    }

    #[test]
    fn an_empty_field_answers_nothing() {
        let chip = Chip::new(std::vec![None, None]);
        let mut reader = St25r3916::new(chip, Delay, ADDRESS).expect("address");
        block_on(reader.initialize()).expect("initialize");
        assert_eq!(block_on(activate(&mut reader)), Ok(None));
        let (chip, _) = reader.release();
        assert_eq!(chip.sent, [std::vec![0x26], std::vec![0x52]]);
    }
}
