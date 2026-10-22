//! LoRa radio on a Semtech SX1262 whose reset, and optionally its RF switch
//! and LNA enables, sit on PI4IOE5V6408 expander outputs, as on the M5Stack
//! Unit C6L.
//!
//! Initialization switches the RF switch and LNA enables on and pulses the
//! reset, as M5Stack's Meshtastic variant for the Unit C6L does
//! (`c6l_init`: P5 LNA enable and P6 RF switch high, P7 reset), then prepares
//! the chip: its TCXO on DIO3 and DIO2 as the antenna switch where the Board
//! says so. The radio stays in standby until configured. A transmission waits
//! for DIO1 within twice the packet's time on air; a reception waits for
//! DIO1 as long as it takes and restarts after a packet with a bad header or
//! CRC.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_driver_sx126x::{
    self as sx126x, Bandwidth as ChipBandwidth, LoraSettings, Sx126x, irq,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    lora::{Bandwidth, Lora, LoraConfig, MAX_PAYLOAD_BYTES, RadioLimits, Received},
};
use embassy_time::{Delay, Duration, Timer, with_timeout};
use embedded_hal::{digital::InputPin, i2c::I2c, spi::SpiDevice};
use embedded_hal_async::digital::Wait;

pub use barracuda_driver_sx126x::TcxoVoltage;

/// How long the reset line is held low; the datasheet asks for 100 µs.
const RESET_LOW_MS: u64 = 1;
/// Margin added to twice the time on air before a transmission fails.
const TRANSMIT_MARGIN_MS: u64 = 100;

/// Board-owned expander pins, reference oscillator, RF switching, and band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sx1262LoraConfig {
    expander_address: u8,
    reset_pin: u8,
    rf_switch_pin: Option<u8>,
    lna_pin: Option<u8>,
    tcxo: Option<TcxoVoltage>,
    tcxo_startup_ms: u32,
    dio2_rf_switch: bool,
    min_frequency_hz: u32,
    max_frequency_hz: u32,
}

impl Sx1262LoraConfig {
    /// Creates a configuration.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        expander_address: u8,
        reset_pin: u8,
        rf_switch_pin: Option<u8>,
        lna_pin: Option<u8>,
        tcxo: Option<TcxoVoltage>,
        tcxo_startup_ms: i64,
        dio2_rf_switch: bool,
        min_frequency_hz: i64,
        max_frequency_hz: i64,
    ) -> Self {
        Self {
            expander_address,
            reset_pin,
            rf_switch_pin,
            lna_pin,
            tcxo,
            tcxo_startup_ms: u32::try_from(tcxo_startup_ms).unwrap_or(0),
            dio2_rf_switch,
            min_frequency_hz: u32::try_from(min_frequency_hz)
                .unwrap_or(sx126x::MIN_FREQUENCY_HZ)
                .max(sx126x::MIN_FREQUENCY_HZ),
            max_frequency_hz: u32::try_from(max_frequency_hz)
                .unwrap_or(sx126x::MAX_FREQUENCY_HZ)
                .min(sx126x::MAX_FREQUENCY_HZ),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Sx1262LoraBindings<SPI, BUSY, DIO1, I2C> {
    spi: SPI,
    busy: BUSY,
    dio1: DIO1,
    expander: I2C,
}

impl<SPI, BUSY, DIO1, I2C> Sx1262LoraBindings<SPI, BUSY, DIO1, I2C> {
    /// Collects the radio's SPI device, BUSY and DIO1 inputs, and the
    /// expander's I2C device.
    #[must_use]
    pub const fn new(spi: SPI, busy: BUSY, dio1: DIO1, expander: I2C) -> Self {
        Self {
            spi,
            busy,
            dio1,
            expander,
        }
    }
}

/// Radio failure.
#[derive(Debug)]
pub enum Sx1262LoraError<SpiError, BusyError, Dio1Error, I2cError> {
    /// The radio or its SPI device failed.
    Radio(sx126x::Error<SpiError, BusyError>),
    /// The DIO1 line could not be read.
    Dio1(Dio1Error),
    /// The expander that holds the reset and RF enables failed.
    Expander(ExpanderError<I2cError>),
    /// The radio has not been configured yet.
    NotConfigured,
    /// A setting is outside the radio's or the Board's range.
    InvalidConfig,
    /// The payload is empty or longer than 255 bytes.
    InvalidPayload,
    /// The transmission did not end in time.
    TransmitTimeout,
}

type ErrorOf<SPI, BUSY, DIO1, I2C> = Sx1262LoraError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <BUSY as embedded_hal::digital::ErrorType>::Error,
    <DIO1 as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

impl<SpiError, BusyError, Dio1Error, I2cError> From<sx126x::Error<SpiError, BusyError>>
    for Sx1262LoraError<SpiError, BusyError, Dio1Error, I2cError>
{
    fn from(error: sx126x::Error<SpiError, BusyError>) -> Self {
        match error {
            sx126x::Error::InvalidSettings => Self::InvalidConfig,
            error => Self::Radio(error),
        }
    }
}

impl<SpiError, BusyError, Dio1Error, I2cError> From<ExpanderError<I2cError>>
    for Sx1262LoraError<SpiError, BusyError, Dio1Error, I2cError>
{
    fn from(error: ExpanderError<I2cError>) -> Self {
        Self::Expander(error)
    }
}

/// Initialized LoRa radio.
pub struct Sx1262Lora<SPI, BUSY, DIO1, I2C> {
    radio: Sx126x<SPI, BUSY, Delay>,
    dio1: DIO1,
    _expander: I2C,
    limits: RadioLimits,
    settings: Option<LoraSettings>,
}

/// Static factory used by generated Board composition.
pub struct Sx1262LoraImplementation<SPI, BUSY, DIO1, I2C>(Marker<SPI, BUSY, DIO1, I2C>);

/// Names the bindings without owning them.
type Marker<SPI, BUSY, DIO1, I2C> = PhantomData<fn() -> (SPI, BUSY, DIO1, I2C)>;

impl<SPI, BUSY, DIO1, I2C> PeripheralImplementation
    for Sx1262LoraImplementation<SPI, BUSY, DIO1, I2C>
where
    SPI: SpiDevice + 'static,
    BUSY: InputPin + 'static,
    DIO1: InputPin + Wait + 'static,
    I2C: I2c + 'static,
{
    type Bindings = Sx1262LoraBindings<SPI, BUSY, DIO1, I2C>;
    type Config = Sx1262LoraConfig;
    type Peripheral = Sx1262Lora<SPI, BUSY, DIO1, I2C>;
    type Error = ErrorOf<SPI, BUSY, DIO1, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut expander_bus = bindings.expander;
        {
            let mut expander = Pi4ioe5v6408::new(&mut expander_bus, config.expander_address)?;
            for pin in [config.rf_switch_pin, config.lna_pin].into_iter().flatten() {
                expander.configure_output(pin, true)?;
            }
            expander.configure_output(config.reset_pin, false)?;
        }
        Timer::after_millis(RESET_LOW_MS).await;
        Pi4ioe5v6408::new(&mut expander_bus, config.expander_address)?
            .set_output(config.reset_pin, true)?;
        let mut radio = Sx126x::new(bindings.spi, bindings.busy, Delay);
        radio.wait_ready().await?;
        radio
            .initialize(
                config.tcxo.map(|voltage| (voltage, config.tcxo_startup_ms)),
                config.dio2_rf_switch,
            )
            .await?;
        Ok(Sx1262Lora {
            radio,
            dio1: bindings.dio1,
            _expander: expander_bus,
            limits: RadioLimits {
                min_frequency_hz: config.min_frequency_hz,
                max_frequency_hz: config.max_frequency_hz,
                min_tx_power_dbm: sx126x::MIN_POWER_DBM,
                max_tx_power_dbm: sx126x::MAX_POWER_DBM,
            },
            settings: None,
        })
    }
}

const fn chip_bandwidth(bandwidth: Bandwidth) -> ChipBandwidth {
    match bandwidth {
        Bandwidth::Khz7_8 => ChipBandwidth::Khz7_8,
        Bandwidth::Khz10_4 => ChipBandwidth::Khz10_4,
        Bandwidth::Khz15_6 => ChipBandwidth::Khz15_6,
        Bandwidth::Khz20_8 => ChipBandwidth::Khz20_8,
        Bandwidth::Khz31_25 => ChipBandwidth::Khz31_25,
        Bandwidth::Khz41_7 => ChipBandwidth::Khz41_7,
        Bandwidth::Khz62_5 => ChipBandwidth::Khz62_5,
        Bandwidth::Khz125 => ChipBandwidth::Khz125,
        Bandwidth::Khz250 => ChipBandwidth::Khz250,
        Bandwidth::Khz500 => ChipBandwidth::Khz500,
    }
}

impl<SPI, BUSY, DIO1, I2C> Sx1262Lora<SPI, BUSY, DIO1, I2C>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DIO1: InputPin + Wait,
    I2C: I2c,
{
    fn settings(&self) -> Result<LoraSettings, ErrorOf<SPI, BUSY, DIO1, I2C>> {
        self.settings.ok_or(Sx1262LoraError::NotConfigured)
    }

    /// Waits for DIO1, then reads and clears the interrupt sources.
    async fn wait_irq(&mut self) -> Result<u16, ErrorOf<SPI, BUSY, DIO1, I2C>> {
        self.dio1
            .wait_for_high()
            .await
            .map_err(Sx1262LoraError::Dio1)?;
        let status = self.radio.irq_status().await?;
        self.radio.clear_irq(irq::ALL).await?;
        Ok(status)
    }
}

impl<SPI, BUSY, DIO1, I2C> Lora for Sx1262Lora<SPI, BUSY, DIO1, I2C>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DIO1: InputPin + Wait,
    I2C: I2c,
{
    type Error = ErrorOf<SPI, BUSY, DIO1, I2C>;

    fn limits(&self) -> RadioLimits {
        self.limits
    }

    async fn configure(&mut self, config: LoraConfig) -> Result<(), Self::Error> {
        if !(self.limits.min_frequency_hz..=self.limits.max_frequency_hz)
            .contains(&config.frequency_hz)
        {
            return Err(Sx1262LoraError::InvalidConfig);
        }
        let settings = LoraSettings {
            frequency_hz: config.frequency_hz,
            spreading_factor: config.spreading_factor,
            bandwidth: chip_bandwidth(config.bandwidth),
            coding_rate: config.coding_rate.denominator(),
            power_dbm: config.tx_power_dbm,
            preamble_symbols: config.preamble_symbols,
            sync_word: config.sync_word,
            crc: config.crc,
        };
        self.settings = None;
        self.radio.configure(&settings).await?;
        self.settings = Some(settings);
        Ok(())
    }

    async fn transmit(&mut self, payload: &[u8]) -> Result<(), Self::Error> {
        let settings = self.settings()?;
        let len = u8::try_from(payload.len())
            .ok()
            .filter(|len| *len > 0 && usize::from(*len) <= MAX_PAYLOAD_BYTES)
            .ok_or(Sx1262LoraError::InvalidPayload)?;
        self.radio.start_transmit(&settings, payload).await?;
        let limit = Duration::from_micros(settings.time_on_air_us(len).saturating_mul(2))
            .checked_add(Duration::from_millis(TRANSMIT_MARGIN_MS))
            .unwrap_or(Duration::MAX);
        match with_timeout(limit, self.wait_irq()).await {
            Ok(status) => {
                let status = status?;
                if status & irq::TX_DONE != 0 {
                    Ok(())
                } else {
                    Err(Sx1262LoraError::TransmitTimeout)
                }
            }
            Err(_) => {
                self.radio.standby().await?;
                Err(Sx1262LoraError::TransmitTimeout)
            }
        }
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<Received, Self::Error> {
        let settings = self.settings()?;
        loop {
            self.radio.start_receive(&settings).await?;
            let status = self.wait_irq().await?;
            if status & irq::RX_DONE != 0 && status & (irq::CRC_ERROR | irq::HEADER_ERROR) == 0 {
                let packet = self.radio.read_packet(buffer).await?;
                return Ok(Received {
                    len: usize::from(packet.len).min(buffer.len()),
                    rssi_dbm: packet.rssi_dbm,
                    snr_centi_db: i16::from(packet.snr_quarter_db).saturating_mul(25),
                });
            }
            // A damaged packet: listen again.
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects, clippy::panic)]

    extern crate std;

    use core::convert::Infallible;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        vec::Vec,
    };

    use barracuda_peripheral::{
        PeripheralImplementation,
        lora::{Lora, LoraConfig},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        digital::{ErrorType as PinErrorType, InputPin},
        i2c::{self, I2c},
        spi::{self, Operation, SpiDevice},
    };
    use embedded_hal_async::digital::Wait;

    use super::{
        Sx1262LoraBindings, Sx1262LoraConfig, Sx1262LoraError, Sx1262LoraImplementation,
        TcxoVoltage,
    };

    /// Shared record of what happened, in order.
    type Log = Rc<RefCell<Vec<std::string::String>>>;

    /// The radio: records opcodes and answers interrupt and packet queries.
    struct Radio {
        log: Log,
        irq: Rc<Cell<u16>>,
        buffer: Vec<u8>,
    }

    impl spi::ErrorType for Radio {
        type Error = Infallible;
    }

    impl SpiDevice for Radio {
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
                0x0e => {
                    self.buffer = sent[2..].to_vec();
                    Vec::new()
                }
                0x12 => std::vec![0, (self.irq.get() >> 8) as u8, self.irq.get() as u8],
                0x13 => std::vec![0, self.buffer.len() as u8, 0],
                0x14 => std::vec![0, 90, 40, 90],
                0x1e => {
                    let mut answer = std::vec![0];
                    answer.extend_from_slice(&self.buffer);
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
            self.log
                .borrow_mut()
                .push(std::format!("spi {:02x}", sent[0]));
            Ok(())
        }
    }

    struct Low;

    impl PinErrorType for Low {
        type Error = Infallible;
    }

    impl InputPin for Low {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(false)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            Ok(true)
        }
    }

    /// DIO1, which the radio raises at once.
    struct Dio1;

    impl PinErrorType for Dio1 {
        type Error = Infallible;
    }

    impl InputPin for Dio1 {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(true)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            Ok(false)
        }
    }

    impl Wait for Dio1 {
        async fn wait_for_high(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
        async fn wait_for_low(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
        async fn wait_for_rising_edge(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
        async fn wait_for_falling_edge(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
        async fn wait_for_any_edge(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    /// The PI4IOE5V6408 at 43h.
    struct Expander {
        log: Log,
        registers: [u8; 0x12],
    }

    impl i2c::ErrorType for Expander {
        type Error = Infallible;
    }

    impl I2c for Expander {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [i2c::Operation<'_>],
        ) -> Result<(), Infallible> {
            assert_eq!(address, 0x43);
            let mut register = 0;
            for operation in operations {
                match operation {
                    i2c::Operation::Write([reg]) => register = usize::from(*reg),
                    i2c::Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        if *reg == 0x05 {
                            self.log.borrow_mut().push(std::format!("out {value:08b}"));
                        }
                    }
                    i2c::Operation::Read(bytes) => bytes.fill(self.registers[register]),
                    i2c::Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    /// M5Stack Unit C6L: reset P7, RF switch P6, LNA P5, a 3.0 V TCXO, and
    /// DIO2 as the antenna switch (Meshtastic `m5stack_unitc6l`).
    fn unit_c6l(
        log: &Log,
        irq: &Rc<Cell<u16>>,
    ) -> (
        Sx1262LoraBindings<Radio, Low, Dio1, Expander>,
        Sx1262LoraConfig,
    ) {
        (
            Sx1262LoraBindings::new(
                Radio {
                    log: Rc::clone(log),
                    irq: Rc::clone(irq),
                    buffer: Vec::new(),
                },
                Low,
                Dio1,
                Expander {
                    log: Rc::clone(log),
                    registers: [0; 0x12],
                },
            ),
            Sx1262LoraConfig::new(
                0x43,
                7,
                Some(6),
                Some(5),
                Some(TcxoVoltage::V3_0),
                5,
                true,
                850_000_000,
                930_000_000,
            ),
        )
    }

    #[test]
    fn enables_the_rf_path_and_resets_the_radio_before_talking_to_it() {
        let log = Log::default();
        let (bindings, config) = unit_c6l(&log, &Rc::default());
        block_on(Sx1262LoraImplementation::initialize(bindings, config)).expect("initialize");
        let log = log.borrow();
        assert_eq!(log[0], "out 01000000");
        assert_eq!(log[1], "out 01100000");
        // Reset held low with both enables on, then released.
        assert_eq!(log[2], "out 01100000");
        assert_eq!(log[3], "out 11100000");
        assert_eq!(log[4], "spi 80");
        assert!(
            log.contains(&std::string::String::from("spi 97")),
            "TCXO on DIO3"
        );
        assert!(
            log.contains(&std::string::String::from("spi 9d")),
            "DIO2 RF switch"
        );
    }

    #[test]
    fn needs_a_configuration_inside_the_band_then_sends_and_receives() {
        let log = Log::default();
        let irq = Rc::new(Cell::new(0));
        let (bindings, config) = unit_c6l(&log, &irq);
        let mut lora =
            block_on(Sx1262LoraImplementation::initialize(bindings, config)).expect("initialize");
        assert!(matches!(
            block_on(lora.transmit(b"hi")),
            Err(Sx1262LoraError::NotConfigured)
        ));
        assert!(matches!(
            block_on(lora.configure(LoraConfig::new(433_000_000))),
            Err(Sx1262LoraError::InvalidConfig)
        ));
        block_on(lora.configure(LoraConfig::new(868_000_000))).expect("configure");
        irq.set(super::irq::TX_DONE);
        block_on(lora.transmit(b"hello")).expect("transmit");
        assert!(matches!(
            block_on(lora.transmit(&[])),
            Err(Sx1262LoraError::InvalidPayload)
        ));
        irq.set(super::irq::RX_DONE);
        let mut buffer = [0; 16];
        let received = block_on(lora.receive(&mut buffer)).expect("receive");
        assert_eq!(&buffer[..received.len], b"hello");
        assert_eq!(received.rssi_dbm, -45);
        assert_eq!(received.snr_centi_db, 1000);
    }
}
