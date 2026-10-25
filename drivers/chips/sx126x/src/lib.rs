//! Low-level Driver for one Semtech SX1262 LoRa transceiver on SPI.
//!
//! Opcodes, register addresses, and sequences follow the SX1261/2 datasheet
//! (rev. 2.1): every command waits for BUSY low first; a TCXO on DIO3 is
//! started with `SetDIO3AsTCXOCtrl` and followed by a full calibration;
//! DIO2 may switch the RF path itself; the high-power PA is set up as for
//! +22 dBm (`SetPaConfig 04h 07h 00h 01h`), and `SetTxParams` then sets the
//! power. The workarounds of datasheet chapter 15 are applied: the
//! modulation quality fix for 500 kHz (register `0889h` bit 2), the TX clamp
//! (`08D8h` bits 4:1), and the IQ polarity fix (`0736h` bit 2).
//!
//! The driver only issues commands. Waiting for DIO1 and choosing time-outs
//! belong to the caller.

#![no_std]

use embedded_hal::{
    digital::InputPin,
    spi::{Operation, SpiDevice},
};
use embedded_hal_async::delay::DelayNs;

const SET_STANDBY: u8 = 0x80;
const SET_RX: u8 = 0x82;
const SET_TX: u8 = 0x83;
const SET_RF_FREQUENCY: u8 = 0x86;
const CALIBRATE: u8 = 0x89;
const SET_PACKET_TYPE: u8 = 0x8a;
const SET_MODULATION_PARAMS: u8 = 0x8b;
const SET_PACKET_PARAMS: u8 = 0x8c;
const SET_TX_PARAMS: u8 = 0x8e;
const SET_BUFFER_BASE_ADDRESS: u8 = 0x8f;
const SET_PA_CONFIG: u8 = 0x95;
const SET_REGULATOR_MODE: u8 = 0x96;
const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
const CALIBRATE_IMAGE: u8 = 0x98;
const SET_DIO2_AS_RF_SWITCH_CTRL: u8 = 0x9d;
const SET_DIO_IRQ_PARAMS: u8 = 0x08;
const CLEAR_IRQ_STATUS: u8 = 0x02;
const CLEAR_DEVICE_ERRORS: u8 = 0x07;
const WRITE_REGISTER: u8 = 0x0d;
const WRITE_BUFFER: u8 = 0x0e;
const GET_IRQ_STATUS: u8 = 0x12;
const GET_RX_BUFFER_STATUS: u8 = 0x13;
const GET_PACKET_STATUS: u8 = 0x14;
const READ_REGISTER: u8 = 0x1d;
const READ_BUFFER: u8 = 0x1e;

const REG_IQ_POLARITY: u16 = 0x0736;
const REG_LORA_SYNC_WORD: u16 = 0x0740;
const REG_TX_MODULATION: u16 = 0x0889;
const REG_TX_CLAMP: u16 = 0x08d8;

/// `SetStandby` with the 13 MHz RC oscillator.
const STANDBY_RC: u8 = 0x00;
/// `SetRegulatorMode`: the DC-DC converter.
const REGULATOR_DC_DC: u8 = 0x01;
/// `SetPacketType`: LoRa.
const PACKET_TYPE_LORA: u8 = 0x01;
/// `Calibrate`: every block.
const CALIBRATE_ALL: u8 = 0x7f;
/// `SetTxParams` ramp time: 200 µs.
const RAMP_200_US: u8 = 0x04;
/// `SetRx` timeout 0: single reception, no time-out.
const RX_SINGLE: u32 = 0;
/// One TCXO start-up time unit: 15.625 µs, as 1/64 ms.
const TCXO_DELAY_UNITS_PER_MS: u32 = 64;
/// Longest BUSY wait: calibration takes 3.5 ms, a TCXO start up to tens of
/// milliseconds.
const BUSY_POLLS: u32 = 2_000;
const BUSY_POLL_US: u32 = 50;
/// The crystal or TCXO frequency the synthesizer steps divide.
const XTAL_HZ: u64 = 32_000_000;

/// Lowest and highest frequency and power of the SX1262 (datasheet 3).
pub const MIN_FREQUENCY_HZ: u32 = 150_000_000;
/// Highest carrier frequency.
pub const MAX_FREQUENCY_HZ: u32 = 960_000_000;
/// Lowest transmit power of the high-power PA, in dBm.
pub const MIN_POWER_DBM: i8 = -9;
/// Highest transmit power of the high-power PA, in dBm.
pub const MAX_POWER_DBM: i8 = 22;

/// Interrupt sources (datasheet table 13-29).
pub mod irq {
    /// The packet was sent.
    pub const TX_DONE: u16 = 1 << 0;
    /// A packet was received.
    pub const RX_DONE: u16 = 1 << 1;
    /// A LoRa header with a wrong CRC was received.
    pub const HEADER_ERROR: u16 = 1 << 5;
    /// A packet with a wrong payload CRC was received.
    pub const CRC_ERROR: u16 = 1 << 6;
    /// The receive or transmit time-out expired.
    pub const TIMEOUT: u16 = 1 << 9;
    /// Every source.
    pub const ALL: u16 = 0x03ff;
}

/// The TCXO supply DIO3 provides (datasheet table 13-35).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcxoVoltage {
    /// 1.6 V.
    V1_6,
    /// 1.7 V.
    V1_7,
    /// 1.8 V.
    V1_8,
    /// 2.2 V.
    V2_2,
    /// 2.4 V.
    V2_4,
    /// 2.7 V.
    V2_7,
    /// 3.0 V.
    V3_0,
    /// 3.3 V.
    V3_3,
}

impl TcxoVoltage {
    const fn code(self) -> u8 {
        match self {
            Self::V1_6 => 0x00,
            Self::V1_7 => 0x01,
            Self::V1_8 => 0x02,
            Self::V2_2 => 0x03,
            Self::V2_4 => 0x04,
            Self::V2_7 => 0x05,
            Self::V3_0 => 0x06,
            Self::V3_3 => 0x07,
        }
    }
}

/// LoRa bandwidth with its `SetModulationParams` code (datasheet table 13-48).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bandwidth {
    /// 7.81 kHz.
    Khz7_8,
    /// 10.42 kHz.
    Khz10_4,
    /// 15.63 kHz.
    Khz15_6,
    /// 20.83 kHz.
    Khz20_8,
    /// 31.25 kHz.
    Khz31_25,
    /// 41.67 kHz.
    Khz41_7,
    /// 62.5 kHz.
    Khz62_5,
    /// 125 kHz.
    Khz125,
    /// 250 kHz.
    Khz250,
    /// 500 kHz.
    Khz500,
}

impl Bandwidth {
    const fn code(self) -> u8 {
        match self {
            Self::Khz7_8 => 0x00,
            Self::Khz10_4 => 0x08,
            Self::Khz15_6 => 0x01,
            Self::Khz20_8 => 0x09,
            Self::Khz31_25 => 0x02,
            Self::Khz41_7 => 0x0a,
            Self::Khz62_5 => 0x03,
            Self::Khz125 => 0x04,
            Self::Khz250 => 0x05,
            Self::Khz500 => 0x06,
        }
    }

    /// Returns the bandwidth in hertz, rounded to the nearest 10 Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Khz7_8 => 7_810,
            Self::Khz10_4 => 10_420,
            Self::Khz15_6 => 15_630,
            Self::Khz20_8 => 20_830,
            Self::Khz31_25 => 31_250,
            Self::Khz41_7 => 41_670,
            Self::Khz62_5 => 62_500,
            Self::Khz125 => 125_000,
            Self::Khz250 => 250_000,
            Self::Khz500 => 500_000,
        }
    }
}

/// LoRa modulation and packet settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoraSettings {
    /// Carrier frequency in hertz.
    pub frequency_hz: u32,
    /// Spreading factor, 5 through 12.
    pub spreading_factor: u8,
    /// Bandwidth.
    pub bandwidth: Bandwidth,
    /// Coding rate denominator: 4/5 through 4/8 as 5 through 8.
    pub coding_rate: u8,
    /// Transmit power in dBm, -9 through 22.
    pub power_dbm: i8,
    /// Preamble length in symbols.
    pub preamble_symbols: u16,
    /// Network sync word, `12h` private or `34h` LoRaWAN.
    pub sync_word: u8,
    /// Whether packets carry a payload CRC.
    pub crc: bool,
}

impl LoraSettings {
    /// Returns whether a symbol lasts longer than 16.38 ms, which needs the
    /// low-data-rate optimization (datasheet 6.1.1.4).
    #[must_use]
    pub fn low_data_rate(&self) -> bool {
        let symbol_us = (1_u64 << self.spreading_factor.min(12))
            .saturating_mul(1_000_000)
            .checked_div(u64::from(self.bandwidth.hz()))
            .unwrap_or(u64::MAX);
        symbol_us > 16_380
    }

    /// Returns an upper bound of the time on air of a `payload_len`-byte
    /// packet with an explicit header, in microseconds (Semtech's LoRa
    /// formula, AN1200.13, with 4.25 symbols for the sync word and SFD).
    #[must_use]
    pub fn time_on_air_us(&self, payload_len: u8) -> u64 {
        let sf = i64::from(self.spreading_factor.clamp(5, 12));
        let ldro = i64::from(u8::from(self.low_data_rate()));
        let crc = i64::from(u8::from(self.crc));
        let numerator = 8_i64
            .saturating_mul(i64::from(payload_len))
            .saturating_sub(4_i64.saturating_mul(sf))
            .saturating_add(28)
            .saturating_add(16_i64.saturating_mul(crc));
        let denominator = 4_i64.saturating_mul(sf.saturating_sub(2_i64.saturating_mul(ldro)));
        let blocks = if numerator > 0 && denominator > 0 {
            numerator
                .saturating_add(denominator.saturating_sub(1))
                .checked_div(denominator)
                .unwrap_or(0)
        } else {
            0
        };
        let payload_symbols = blocks
            .saturating_mul(i64::from(self.coding_rate.clamp(5, 8)))
            .saturating_add(8);
        // Symbols in quarters: preamble + 4.25, then the payload.
        let quarter_symbols = i64::from(self.preamble_symbols)
            .saturating_mul(4)
            .saturating_add(17)
            .saturating_add(payload_symbols.saturating_mul(4));
        let symbol_us = (1_u64 << sf)
            .saturating_mul(1_000_000)
            .div_ceil(u64::from(self.bandwidth.hz()));
        u64::try_from(quarter_symbols)
            .unwrap_or(u64::MAX)
            .saturating_mul(symbol_us)
            .div_ceil(4)
    }
}

/// One received packet's place in the buffer and its signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketStatus {
    /// Payload length.
    pub len: u8,
    /// Received signal strength of the packet in dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio in quarters of a dB.
    pub snr_quarter_db: i8,
}

/// SX1262 command or bus failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, PinError> {
    /// BUSY stayed high.
    Busy,
    /// A setting is outside the chip's range.
    InvalidSettings,
    /// The SPI device failed.
    Spi(SpiError),
    /// The BUSY pin could not be read.
    Pin(PinError),
}

/// One SX1262 with its BUSY line and delay source.
pub struct Sx126x<SPI, BUSY, D> {
    spi: SPI,
    busy: BUSY,
    delay: D,
}

type Result<T, SPI, BUSY> = core::result::Result<
    T,
    Error<
        <SPI as embedded_hal::spi::ErrorType>::Error,
        <BUSY as embedded_hal::digital::ErrorType>::Error,
    >,
>;

impl<SPI, BUSY, D> Sx126x<SPI, BUSY, D>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    D: DelayNs,
{
    /// Wraps the chip's SPI device, BUSY line, and a delay source.
    pub const fn new(spi: SPI, busy: BUSY, delay: D) -> Self {
        Self { spi, busy, delay }
    }

    /// Returns the SPI device, BUSY line, and delay.
    pub fn release(self) -> (SPI, BUSY, D) {
        (self.spi, self.busy, self.delay)
    }

    /// Waits until the chip accepts a command.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when BUSY stays high, or a pin error.
    pub async fn wait_ready(&mut self) -> Result<(), SPI, BUSY> {
        for _ in 0..BUSY_POLLS {
            if self.busy.is_low().map_err(Error::Pin)? {
                return Ok(());
            }
            self.delay.delay_us(BUSY_POLL_US).await;
        }
        Err(Error::Busy)
    }

    /// Sends one command with its parameters.
    async fn command(&mut self, opcode: u8, parameters: &[u8]) -> Result<(), SPI, BUSY> {
        self.wait_ready().await?;
        self.spi
            .transaction(&mut [Operation::Write(&[opcode]), Operation::Write(parameters)])
            .map_err(Error::Spi)
    }

    /// Sends one command and reads its answer after the status byte.
    async fn read_command(
        &mut self,
        opcode: u8,
        parameters: &[u8],
        answer: &mut [u8],
    ) -> Result<(), SPI, BUSY> {
        self.wait_ready().await?;
        let mut status = [0];
        self.spi
            .transaction(&mut [
                Operation::Write(&[opcode]),
                Operation::Write(parameters),
                Operation::Read(&mut status),
                Operation::Read(answer),
            ])
            .map_err(Error::Spi)
    }

    async fn write_register(&mut self, address: u16, value: u8) -> Result<(), SPI, BUSY> {
        let [high, low] = address.to_be_bytes();
        self.command(WRITE_REGISTER, &[high, low, value]).await
    }

    async fn read_register(&mut self, address: u16) -> Result<u8, SPI, BUSY> {
        let [high, low] = address.to_be_bytes();
        let mut value = [0];
        self.read_command(READ_REGISTER, &[high, low], &mut value)
            .await?;
        Ok(value[0])
    }

    async fn update_register(
        &mut self,
        address: u16,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), SPI, BUSY> {
        let value = self.read_register(address).await?;
        self.write_register(address, update(value)).await
    }

    /// Returns the chip to standby on its RC oscillator, ending any
    /// reception or transmission.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn standby(&mut self) -> Result<(), SPI, BUSY> {
        self.command(SET_STANDBY, &[STANDBY_RC]).await
    }

    /// Prepares the chip after reset for LoRa: standby, the TCXO on DIO3
    /// when there is one (started within `tcxo_startup_ms`) and a full
    /// calibration, DIO2 as the RF switch when wired so, the DC-DC
    /// regulator, LoRa packets, the TX clamp fix, both buffers from address
    /// 0, and DIO1 raised by TX done, RX done, CRC and header errors, and
    /// time-out.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn initialize(
        &mut self,
        tcxo: Option<(TcxoVoltage, u32)>,
        dio2_rf_switch: bool,
    ) -> Result<(), SPI, BUSY> {
        self.standby().await?;
        if let Some((voltage, startup_ms)) = tcxo {
            let [_, high, middle, low] = startup_ms
                .saturating_mul(TCXO_DELAY_UNITS_PER_MS)
                .min(0x00ff_ffff)
                .to_be_bytes();
            self.command(SET_DIO3_AS_TCXO_CTRL, &[voltage.code(), high, middle, low])
                .await?;
            // A TCXO set after reset leaves XOSC_START_ERR; recalibrate
            // every block on the new reference (datasheet 13.3.6).
            self.command(CLEAR_DEVICE_ERRORS, &[0x00, 0x00]).await?;
            self.command(CALIBRATE, &[CALIBRATE_ALL]).await?;
            self.wait_ready().await?;
        }
        if dio2_rf_switch {
            self.command(SET_DIO2_AS_RF_SWITCH_CTRL, &[0x01]).await?;
        }
        self.command(SET_REGULATOR_MODE, &[REGULATOR_DC_DC]).await?;
        self.command(SET_PACKET_TYPE, &[PACKET_TYPE_LORA]).await?;
        // Datasheet 15.2: better resistance to antenna mismatch.
        self.update_register(REG_TX_CLAMP, |value| value | 0x1e)
            .await?;
        self.command(SET_BUFFER_BASE_ADDRESS, &[0x00, 0x00]).await?;
        let mask =
            (irq::TX_DONE | irq::RX_DONE | irq::CRC_ERROR | irq::HEADER_ERROR | irq::TIMEOUT)
                .to_be_bytes();
        self.command(
            SET_DIO_IRQ_PARAMS,
            &[mask[0], mask[1], mask[0], mask[1], 0, 0, 0, 0],
        )
        .await
    }

    /// Applies frequency, image calibration, power, modulation, packet
    /// framing, and sync word, from standby.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSettings`] outside the chip's ranges, or a bus
    /// or BUSY failure.
    pub async fn configure(&mut self, settings: &LoraSettings) -> Result<(), SPI, BUSY> {
        if !(MIN_FREQUENCY_HZ..=MAX_FREQUENCY_HZ).contains(&settings.frequency_hz)
            || !(5..=12).contains(&settings.spreading_factor)
            || !(5..=8).contains(&settings.coding_rate)
            || !(MIN_POWER_DBM..=MAX_POWER_DBM).contains(&settings.power_dbm)
        {
            return Err(Error::InvalidSettings);
        }
        self.standby().await?;
        self.command(CALIBRATE_IMAGE, &image_calibration(settings.frequency_hz))
            .await?;
        self.wait_ready().await?;
        self.command(
            SET_RF_FREQUENCY,
            &frequency_word(settings.frequency_hz).to_be_bytes(),
        )
        .await?;
        self.command(SET_PA_CONFIG, &[0x04, 0x07, 0x00, 0x01])
            .await?;
        self.command(
            SET_TX_PARAMS,
            &[settings.power_dbm.to_be_bytes()[0], RAMP_200_US],
        )
        .await?;
        let coding_rate = settings.coding_rate.saturating_sub(4);
        self.command(
            SET_MODULATION_PARAMS,
            &[
                settings.spreading_factor,
                settings.bandwidth.code(),
                coding_rate,
                u8::from(settings.low_data_rate()),
            ],
        )
        .await?;
        // Datasheet 15.1: bit 2 clear only at 500 kHz.
        let narrow = settings.bandwidth != Bandwidth::Khz500;
        self.update_register(REG_TX_MODULATION, |value| {
            if narrow { value | 0x04 } else { value & !0x04 }
        })
        .await?;
        self.write_packet_params(settings, u8::MAX).await?;
        // Datasheet 15.4: standard IQ sets bit 2.
        self.update_register(REG_IQ_POLARITY, |value| value | 0x04)
            .await?;
        let [msb, lsb] = sync_word_registers(settings.sync_word);
        self.write_register(REG_LORA_SYNC_WORD, msb).await?;
        self.write_register(REG_LORA_SYNC_WORD.saturating_add(1), lsb)
            .await
    }

    async fn write_packet_params(
        &mut self,
        settings: &LoraSettings,
        payload_len: u8,
    ) -> Result<(), SPI, BUSY> {
        let [high, low] = settings.preamble_symbols.to_be_bytes();
        // Explicit header, standard IQ.
        self.command(
            SET_PACKET_PARAMS,
            &[high, low, 0x00, payload_len, u8::from(settings.crc), 0x00],
        )
        .await
    }

    /// Loads `payload` and starts sending it; DIO1 rises at TX done.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSettings`] for an empty or over-long payload,
    /// or a bus or BUSY failure.
    pub async fn start_transmit(
        &mut self,
        settings: &LoraSettings,
        payload: &[u8],
    ) -> Result<(), SPI, BUSY> {
        let len = u8::try_from(payload.len()).map_err(|_| Error::InvalidSettings)?;
        if len == 0 {
            return Err(Error::InvalidSettings);
        }
        self.standby().await?;
        self.write_packet_params(settings, len).await?;
        self.wait_ready().await?;
        self.spi
            .transaction(&mut [
                Operation::Write(&[WRITE_BUFFER, 0x00]),
                Operation::Write(payload),
            ])
            .map_err(Error::Spi)?;
        self.clear_irq(irq::ALL).await?;
        // No chip time-out: the caller bounds the wait.
        self.command(SET_TX, &[0x00, 0x00, 0x00]).await
    }

    /// Starts one reception without time-out; DIO1 rises at RX done or a
    /// CRC or header error.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn start_receive(&mut self, settings: &LoraSettings) -> Result<(), SPI, BUSY> {
        self.standby().await?;
        self.write_packet_params(settings, u8::MAX).await?;
        self.clear_irq(irq::ALL).await?;
        let [_, high, middle, low] = RX_SINGLE.to_be_bytes();
        self.command(SET_RX, &[high, middle, low]).await
    }

    /// Returns the pending interrupt sources.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn irq_status(&mut self) -> Result<u16, SPI, BUSY> {
        let mut status = [0; 2];
        self.read_command(GET_IRQ_STATUS, &[], &mut status).await?;
        Ok(u16::from_be_bytes(status))
    }

    /// Clears the interrupt sources in `mask`.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn clear_irq(&mut self, mask: u16) -> Result<(), SPI, BUSY> {
        self.command(CLEAR_IRQ_STATUS, &mask.to_be_bytes()).await
    }

    /// Copies the received packet into `buffer`, truncated to it, and
    /// returns its length and signal.
    ///
    /// # Errors
    ///
    /// Returns a bus or BUSY failure.
    pub async fn read_packet(&mut self, buffer: &mut [u8]) -> Result<PacketStatus, SPI, BUSY> {
        let mut status = [0; 2];
        self.read_command(GET_RX_BUFFER_STATUS, &[], &mut status)
            .await?;
        let [len, start] = status;
        let copied = usize::from(len).min(buffer.len());
        self.wait_ready().await?;
        let mut nop = [0];
        self.spi
            .transaction(&mut [
                Operation::Write(&[READ_BUFFER, start]),
                Operation::Read(&mut nop),
                Operation::Read(buffer.get_mut(..copied).unwrap_or(&mut [])),
            ])
            .map_err(Error::Spi)?;
        let mut packet = [0; 3];
        self.read_command(GET_PACKET_STATUS, &[], &mut packet)
            .await?;
        Ok(PacketStatus {
            len,
            // RssiPkt is -2 x the RSSI in dBm.
            rssi_dbm: 0_i16.saturating_sub(i16::from(packet[0] >> 1)),
            snr_quarter_db: i8::from_be_bytes([packet[1]]),
        })
    }
}

/// The synthesizer word of `frequency_hz`: `frequency * 2^25 / 32 MHz`.
#[must_use]
pub fn frequency_word(frequency_hz: u32) -> u32 {
    let word = (u64::from(frequency_hz) << 25) / XTAL_HZ;
    u32::try_from(word).unwrap_or(u32::MAX)
}

/// The `CalibrateImage` band of `frequency_hz`: the datasheet's band when it
/// holds the frequency (table 9-2), otherwise the 4 MHz steps around it.
#[must_use]
pub fn image_calibration(frequency_hz: u32) -> [u8; 2] {
    const BANDS: [(u32, u32, [u8; 2]); 5] = [
        (430_000_000, 440_000_000, [0x6b, 0x6f]),
        (470_000_000, 510_000_000, [0x75, 0x81]),
        (779_000_000, 787_000_000, [0xc1, 0xc5]),
        (863_000_000, 870_000_000, [0xd7, 0xdb]),
        (902_000_000, 928_000_000, [0xe1, 0xe9]),
    ];
    if let Some((_, _, band)) = BANDS
        .iter()
        .find(|(low, high, _)| (*low..=*high).contains(&frequency_hz))
    {
        return *band;
    }
    let mhz = frequency_hz / 1_000_000;
    let low = mhz.saturating_sub(1) / 4;
    let high = mhz.saturating_add(1).div_ceil(4);
    [
        u8::try_from(low).unwrap_or(u8::MAX),
        u8::try_from(high).unwrap_or(u8::MAX),
    ]
}

/// The two sync-word registers of a one-byte LoRa sync word: `12h` gives
/// `14h 24h` and `34h` gives `34h 44h` (datasheet table 12-1).
#[must_use]
pub const fn sync_word_registers(sync_word: u8) -> [u8; 2] {
    [(sync_word & 0xf0) | 0x04, ((sync_word & 0x0f) << 4) | 0x04]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects, clippy::panic)]

    extern crate std;

    use core::convert::Infallible;
    use std::{collections::BTreeMap, vec::Vec};

    use embassy_futures::block_on;
    use embedded_hal::{
        digital::{ErrorType as PinErrorType, InputPin},
        spi::{ErrorType, Operation, SpiDevice},
    };
    use embedded_hal_async::delay::DelayNs;

    use super::{
        Bandwidth, Error, LoraSettings, Sx126x, TcxoVoltage, frequency_word, image_calibration,
        irq, sync_word_registers,
    };

    /// Records commands and models the registers, the buffer, and the
    /// status answers.
    #[derive(Default)]
    struct Chip {
        commands: Vec<Vec<u8>>,
        registers: BTreeMap<u16, u8>,
        buffer: Vec<u8>,
        irq: u16,
        rx: (u8, u8),
        packet: [u8; 3],
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl SpiDevice for Chip {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
            let mut sent = Vec::new();
            let mut reads: Vec<&mut [u8]> = Vec::new();
            for operation in operations.iter_mut() {
                match operation {
                    Operation::Write(bytes) => sent.extend_from_slice(bytes),
                    Operation::Read(bytes) => reads.push(bytes),
                    _ => panic!("unexpected SPI operation"),
                }
            }
            let answer: Vec<u8> = match sent[0] {
                0x0d => {
                    let address = u16::from_be_bytes([sent[1], sent[2]]);
                    self.registers.insert(address, sent[3]);
                    Vec::new()
                }
                0x1d => {
                    let address = u16::from_be_bytes([sent[1], sent[2]]);
                    std::vec![0, *self.registers.get(&address).unwrap_or(&0)]
                }
                0x0e => {
                    self.buffer = sent[2..].to_vec();
                    Vec::new()
                }
                0x12 => std::vec![0, (self.irq >> 8) as u8, self.irq as u8],
                0x13 => std::vec![0, self.rx.0, self.rx.1],
                0x14 => std::vec![0, self.packet[0], self.packet[1], self.packet[2]],
                0x1e => {
                    let start = usize::from(sent[1]);
                    let mut answer = std::vec![0];
                    answer.extend_from_slice(&self.buffer[start..]);
                    answer
                }
                _ => Vec::new(),
            };
            let mut answer = answer.into_iter();
            for read in reads {
                for byte in read.iter_mut() {
                    *byte = answer.next().unwrap_or(0);
                }
            }
            self.commands.push(sent);
            Ok(())
        }
    }

    struct Busy {
        high_polls: u32,
    }

    impl PinErrorType for Busy {
        type Error = Infallible;
    }

    impl InputPin for Busy {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(!self.is_low()?)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            if self.high_polls == 0 {
                Ok(true)
            } else {
                self.high_polls -= 1;
                Ok(false)
            }
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        async fn delay_ns(&mut self, _ns: u32) {}
    }

    fn radio() -> Sx126x<Chip, Busy, Delay> {
        Sx126x::new(Chip::default(), Busy { high_polls: 0 }, Delay)
    }

    /// M5Stack UIFlow's LoRa defaults at 868 MHz.
    fn settings() -> LoraSettings {
        LoraSettings {
            frequency_hz: 868_000_000,
            spreading_factor: 8,
            bandwidth: Bandwidth::Khz250,
            coding_rate: 8,
            power_dbm: 10,
            preamble_symbols: 12,
            sync_word: 0x12,
            crc: true,
        }
    }

    #[test]
    fn computes_frequency_words_bands_and_sync_words() {
        // Datasheet 13.4.1: 868 MHz is 0x36400000.
        assert_eq!(frequency_word(868_000_000), 0x3640_0000);
        assert_eq!(frequency_word(915_000_000), 0x3930_0000);
        assert_eq!(image_calibration(868_000_000), [0xd7, 0xdb]);
        assert_eq!(image_calibration(915_000_000), [0xe1, 0xe9]);
        assert_eq!(image_calibration(923_000_000), [0xe1, 0xe9]);
        assert_eq!(image_calibration(850_000_000), [212, 213]);
        assert_eq!(sync_word_registers(0x12), [0x14, 0x24]);
        assert_eq!(sync_word_registers(0x34), [0x34, 0x44]);
    }

    #[test]
    fn low_data_rate_and_time_on_air() {
        let mut slow = settings();
        assert!(!slow.low_data_rate());
        slow.spreading_factor = 12;
        slow.bandwidth = Bandwidth::Khz125;
        // A 32.768 ms symbol.
        assert!(slow.low_data_rate());
        // SF7, 125 kHz, 4/5, 8 symbols, CRC, 10 bytes: 41.2 ms (Semtech's
        // LoRa calculator gives 41.22 ms).
        let fast = LoraSettings {
            spreading_factor: 7,
            bandwidth: Bandwidth::Khz125,
            coding_rate: 5,
            preamble_symbols: 8,
            ..settings()
        };
        let time = fast.time_on_air_us(10);
        assert!((41_000..41_500).contains(&time), "{time}");
    }

    #[test]
    fn initializes_with_a_tcxo_and_an_rf_switch() {
        let mut radio = radio();
        block_on(radio.initialize(Some((TcxoVoltage::V3_0, 5)), true)).expect("initialize");
        let (chip, _, _) = radio.release();
        let opcodes: Vec<u8> = chip.commands.iter().map(|command| command[0]).collect();
        assert_eq!(
            opcodes,
            [
                0x80, 0x97, 0x07, 0x89, 0x9d, 0x96, 0x8a, 0x1d, 0x0d, 0x8f, 0x08
            ]
        );
        // 3.0 V, 5 ms = 320 units of 15.625 us.
        assert_eq!(chip.commands[1], [0x97, 0x06, 0x00, 0x01, 0x40]);
        assert_eq!(chip.registers[&0x08d8], 0x1e);
        assert_eq!(
            chip.commands[10],
            [0x08, 0x02, 0x63, 0x02, 0x63, 0, 0, 0, 0]
        );
    }

    #[test]
    fn configures_modulation_packets_and_workarounds() {
        let mut radio = radio();
        block_on(radio.configure(&settings())).expect("configure");
        let (chip, _, _) = radio.release();
        let find = |opcode: u8| {
            chip.commands
                .iter()
                .find(|command| command[0] == opcode)
                .expect("command sent")
                .clone()
        };
        assert_eq!(find(0x98), [0x98, 0xd7, 0xdb]);
        assert_eq!(find(0x86), [0x86, 0x36, 0x40, 0x00, 0x00]);
        assert_eq!(find(0x95), [0x95, 0x04, 0x07, 0x00, 0x01]);
        assert_eq!(find(0x8e), [0x8e, 10, 0x04]);
        // SF8, 250 kHz, 4/8, no LDRO.
        assert_eq!(find(0x8b), [0x8b, 8, 0x05, 4, 0]);
        assert_eq!(find(0x8c), [0x8c, 0, 12, 0, 255, 1, 0]);
        assert_eq!(chip.registers[&0x0889], 0x04);
        assert_eq!(chip.registers[&0x0736], 0x04);
        assert_eq!(chip.registers[&0x0740], 0x14);
        assert_eq!(chip.registers[&0x0741], 0x24);
    }

    #[test]
    fn rejects_settings_outside_the_chip() {
        let mut radio = radio();
        let changes: [fn(&mut LoraSettings); 4] = [
            |s: &mut LoraSettings| s.frequency_hz = 100_000_000,
            |s: &mut LoraSettings| s.spreading_factor = 13,
            |s: &mut LoraSettings| s.coding_rate = 9,
            |s: &mut LoraSettings| s.power_dbm = 23,
        ];
        for change in changes {
            let mut bad = settings();
            change(&mut bad);
            assert_eq!(block_on(radio.configure(&bad)), Err(Error::InvalidSettings));
        }
        assert_eq!(
            block_on(radio.start_transmit(&settings(), &[])),
            Err(Error::InvalidSettings)
        );
    }

    #[test]
    fn transmits_and_reads_a_received_packet() {
        let mut radio = radio();
        block_on(radio.start_transmit(&settings(), b"hello")).expect("transmit");
        {
            let chip = &radio.spi;
            assert_eq!(chip.buffer, b"hello");
            let last = chip.commands.last().expect("SetTx");
            assert_eq!(last, &[0x83, 0, 0, 0]);
            assert!(chip.commands.contains(&std::vec![0x8c, 0, 12, 0, 5, 1, 0]));
        }
        radio.spi.irq = irq::RX_DONE;
        radio.spi.rx = (5, 0);
        // RSSI -60 dBm, SNR +7.25 dB.
        radio.spi.packet = [120, 29, 118];
        assert_eq!(block_on(radio.irq_status()), Ok(irq::RX_DONE));
        let mut buffer = [0; 3];
        let status = block_on(radio.read_packet(&mut buffer)).expect("read");
        assert_eq!(status.len, 5);
        assert_eq!(status.rssi_dbm, -60);
        assert_eq!(status.snr_quarter_db, 29);
        assert_eq!(&buffer, b"hel");
    }

    #[test]
    fn waits_for_busy_and_gives_up() {
        let mut radio = Sx126x::new(Chip::default(), Busy { high_polls: 3 }, Delay);
        block_on(radio.standby()).expect("standby after BUSY falls");
        let mut stuck = Sx126x::new(
            Chip::default(),
            Busy {
                high_polls: u32::MAX,
            },
            Delay,
        );
        assert_eq!(block_on(stuck.standby()), Err(Error::Busy));
    }
}
