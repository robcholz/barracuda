// Shared ESP vendor adaptation instantiated inside each supported Platform.

extern crate alloc;

use alloc::boxed::Box;

use barracuda_board_hal::{
    AnalogErrorType, AnalogInput, AnalogVoltageInput, ConfigurableDigitalPin, DigitalLevel,
    InputConfig, OutputConfig, OutputDrive, Pull as BoardPull, RuntimeAnalogPlatform,
    RuntimePlatform, RuntimePwmPlatform, RuntimeUartPlatform, SharedI2cBus, SpiBusControl,
    SpiDeviceControl, UartConfig, UartDataBits, UartParity, UartStopBits, UnavailableAnalogOutput,
};
use core::cell::RefCell;
use embassy_sync::blocking_mutex::CriticalSectionMutex;
use esp_hal::analog::adc::{AdcCalLine, AdcCalScheme, RegisterAccess as AdcRegisterAccess};
use embedded_hal::{
    digital::{ErrorType, StatefulOutputPin},
    i2c::Operation as I2cOperation,
    spi::{Mode as EmbeddedMode, Phase, Polarity},
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcConfig, AdcPin, Attenuation},
    delay::Delay,
    gpio::{
        interconnect::{PeripheralInput, PeripheralOutput},
        AnalogPin, AnyPin, DriveMode, Flex, Input, InputConfig as HalInputConfig, InputPin, Level,
        Output, OutputConfig as HalOutputConfig, OutputPin, Pin, Pull,
    },
    i2c::master::{
        AnyI2c, Config as I2cConfig, ConfigError as I2cConfigErrorInner, I2c,
        Instance as I2cInstance,
    },
    spi::master::{
        AnySpi, Config as SpiConfig, ConfigError as SpiConfigErrorInner, Instance as SpiInstance,
        Spi,
    },
    spi::Mode as HalSpiMode,
    time::Rate,
    uart::{
        AnyUart, Config as HalUartConfig, ConfigError as UartConfigError, DataBits,
        Instance as UartInstance, Parity, StopBits, Uart,
    },
    Blocking,
};

#[cfg(not(feature = "runtime-pwm"))]
use barracuda_board_hal::UnavailablePwm;
#[cfg(feature = "runtime-pwm")]
use esp_hal::ledc::{
    channel::{Channel, ChannelIFace, Error as LedcChannelError, Number as LedcChannelNumber},
    timer::{
        config::{Config as LedcTimerConfig, Duty as LedcDuty},
        Error as LedcTimerError, LSClockSource, Number as LedcTimerNumber, Timer, TimerIFace,
    },
    Ledc, LowSpeed,
};

#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

#[cfg(feature = "runtime-i2s")]
type PlatformI2sResource = RuntimeI2sResource;
#[cfg(not(feature = "runtime-i2s"))]
type PlatformI2sResource = core::convert::Infallible;

#[cfg(feature = "runtime-pwm")]
type PlatformPwmResource = RuntimePwmResource;
#[cfg(not(feature = "runtime-pwm"))]
type PlatformPwmResource = core::convert::Infallible;

#[doc(hidden)]
pub use esp_hal as __vendor;

/// Type-erased push-pull output consumed by peripheral implementations.
pub type DigitalOutput = Output<'static>;
/// Type-erased digital input consumed by peripheral implementations.
pub type DigitalInput = Input<'static>;
/// Blocking SPI bus used by statically selected devices.
///
/// It is esp-hal's blocking driver, which it dereferences to, and also
/// clocks transfers shorter than a byte ([`SpiBusControl`]) through the
/// controller's command phase, which a JTAG port driven through the
/// controller needs.
pub struct SpiBus(Spi<'static, Blocking>);

impl core::ops::Deref for SpiBus {
    type Target = Spi<'static, Blocking>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for SpiBus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl embedded_hal::spi::ErrorType for SpiBus {
    type Error = esp_hal::spi::Error;
}

impl embedded_hal::spi::SpiBus for SpiBus {
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::read(&mut self.0, words)
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::write(&mut self.0, words)
    }

    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer(&mut self.0, read, write)
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer_in_place(&mut self.0, words)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::flush(&mut self.0)
    }
}

impl SpiBusControl for SpiBus {
    fn configure(&mut self, frequency_hz: u32, mode: EmbeddedMode) -> Result<(), Self::Error> {
        let config = SpiConfig::default()
            .with_frequency(Rate::from_hz(frequency_hz))
            .with_mode(hal_spi_mode(mode));
        self.0
            .apply_config(&config)
            .map_err(|_| esp_hal::spi::Error::Unsupported)
    }

    fn write_bits(&mut self, value: u8, bits: u8) -> Result<(), Self::Error> {
        use esp_hal::spi::master::{Address, Command, DataMode};

        let mode = DataMode::SingleTwoDataLines;
        let shift = 8u8.saturating_sub(bits);
        let mask = u8::MAX.checked_shr(u32::from(shift)).unwrap_or(0);
        // The command phase sends its value from bit 7 down to bit 0, then
        // bits 15 to 8 (ESP-IDF `spi_ll_set_command`, MSB first), so a
        // command of up to eight bits starts at bit 7.
        let aligned = u16::from(value & mask) << shift;
        let command = match bits {
            1 => Command::_1Bit(aligned, mode),
            2 => Command::_2Bit(aligned, mode),
            3 => Command::_3Bit(aligned, mode),
            4 => Command::_4Bit(aligned, mode),
            5 => Command::_5Bit(aligned, mode),
            6 => Command::_6Bit(aligned, mode),
            7 => Command::_7Bit(aligned, mode),
            8 => Command::_8Bit(aligned, mode),
            _ => return Err(esp_hal::spi::Error::Unsupported),
        };
        self.0
            .half_duplex_write(mode, command, Address::None, 0, &[])?;
        // A half-duplex transfer leaves the command phase enabled, and the
        // full-duplex transfers that follow do not clear it; an empty
        // transfer without a command, which clocks nothing, does.
        self.0
            .half_duplex_write(mode, Command::None, Address::None, 0, &[])
    }
}

/// Converts an `embedded-hal` SPI mode to esp-hal's.
fn hal_spi_mode(mode: EmbeddedMode) -> HalSpiMode {
    match (mode.polarity, mode.phase) {
        (Polarity::IdleLow, Phase::CaptureOnFirstTransition) => HalSpiMode::_0,
        (Polarity::IdleLow, Phase::CaptureOnSecondTransition) => HalSpiMode::_1,
        (Polarity::IdleHigh, Phase::CaptureOnFirstTransition) => HalSpiMode::_2,
        (Polarity::IdleHigh, Phase::CaptureOnSecondTransition) => HalSpiMode::_3,
    }
}

/// Standards-compliant single-owner SPI device.
///
/// It is `embedded-hal-bus`'s exclusive device, and also lets an
/// implementation change its clock and clock bytes with its chip select
/// released ([`SpiDeviceControl`]), which an SD card in SPI mode needs.
pub struct SpiDevice {
    device: ExclusiveDevice<SpiBus, DigitalOutput, Delay>,
    config: SpiConfig,
    frequency_hz: u32,
}

impl embedded_hal::spi::ErrorType for SpiDevice {
    type Error = embedded_hal_bus::spi::DeviceError<esp_hal::spi::Error, core::convert::Infallible>;
}

impl embedded_hal::spi::SpiDevice for SpiDevice {
    fn transaction(
        &mut self,
        operations: &mut [embedded_hal::spi::Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        self.device.transaction(operations)
    }
}

impl SpiDeviceControl for SpiDevice {
    fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }

    fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error> {
        let config = self.config.with_frequency(Rate::from_hz(frequency_hz));
        self.device
            .bus_mut()
            .apply_config(&config)
            .map_err(|_| embedded_hal_bus::spi::DeviceError::Spi(esp_hal::spi::Error::Unsupported))?;
        self.config = config;
        self.frequency_hz = frequency_hz;
        Ok(())
    }

    fn write_deselected(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        // The exclusive device holds its chip select high between
        // transactions, so writing to the bus directly clocks deselected.
        let bus = self.device.bus_mut();
        embedded_hal::spi::SpiBus::write(bus, bytes)
            .and_then(|()| embedded_hal::spi::SpiBus::flush(bus))
            .map_err(embedded_hal_bus::spi::DeviceError::Spi)
    }
}

/// Static owner of one SPI controller whose clock and data lines several
/// statically selected devices share.
///
/// A transaction takes the bus out of a short critical section and returns
/// it afterwards, so a long display transfer never holds interrupts off.
/// Board peripherals run on one cooperative executor and their transfers are
/// blocking, so two transactions never overlap there; a device that finds the
/// bus taken reports [`SharedSpiError::Busy`] instead of waiting.
pub struct SpiBusManager {
    state: CriticalSectionMutex<RefCell<SharedSpiState>>,
}

struct SharedSpiState {
    bus: Option<SpiBus>,
    /// The device whose configuration the controller holds; 0 for none.
    configured: u32,
    next_device: u32,
}

impl SpiBusManager {
    /// Wraps one exclusively owned bus for chip-select-level sharing.
    #[must_use]
    pub const fn new(bus: SpiBus) -> Self {
        Self {
            state: CriticalSectionMutex::new(RefCell::new(SharedSpiState {
                bus: Some(bus),
                configured: 0,
                next_device: 1,
            })),
        }
    }

    fn register(&self) -> u32 {
        self.state.lock(|state| {
            let mut state = state.borrow_mut();
            let id = state.next_device;
            state.next_device = state.next_device.wrapping_add(1).max(1);
            id
        })
    }

    /// Takes the bus for `device`, telling whether another device's
    /// configuration is applied.
    fn take(&self, device: u32) -> Option<(SpiBus, bool)> {
        self.state.lock(|state| {
            let mut state = state.borrow_mut();
            let bus = state.bus.take()?;
            let reconfigure = state.configured != device;
            Some((bus, reconfigure))
        })
    }

    fn restore(&self, bus: SpiBus, configured: u32) {
        self.state.lock(|state| {
            let mut state = state.borrow_mut();
            state.bus = Some(bus);
            state.configured = configured;
        });
    }

    fn forget_configuration(&self, device: u32) {
        self.state.lock(|state| {
            let mut state = state.borrow_mut();
            if state.configured == device {
                state.configured = 0;
            }
        });
    }
}

/// Statically allocates one shared SPI bus owner at the Board call site.
#[macro_export]
macro_rules! __barracuda_esp_spi_bus_manager {
    ($bus:expr) => {{
        static MANAGER: $crate::hal::__StaticCell<$crate::hal::SpiBusManager> =
            $crate::hal::__StaticCell::new();
        MANAGER.init($crate::hal::SpiBusManager::new($bus))
    }};
}

#[doc(hidden)]
pub use __barracuda_esp_spi_bus_manager as spi_bus_manager;

/// Failure of one device's transaction on a shared SPI bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedSpiError {
    /// Another device's transaction holds the bus.
    Busy,
    /// The controller rejected this device's clock.
    Configuration,
    /// The transfer failed.
    Bus(esp_hal::spi::Error),
}

impl core::fmt::Display for SharedSpiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("the shared SPI bus is in use"),
            Self::Configuration => formatter.write_str("invalid SPI device clock"),
            Self::Bus(_) => formatter.write_str("SPI transfer failed"),
        }
    }
}

impl core::error::Error for SharedSpiError {}

impl embedded_hal::spi::Error for SharedSpiError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        match self {
            Self::Bus(error) => embedded_hal::spi::Error::kind(error),
            Self::Busy | Self::Configuration => embedded_hal::spi::ErrorKind::Other,
        }
    }
}

/// One chip-selected device on a shared SPI controller, at its own clock.
pub struct SharedSpiDevice {
    manager: &'static SpiBusManager,
    chip_select: DigitalOutput,
    config: SpiConfig,
    frequency_hz: u32,
    id: u32,
}

/// Constructs one device on a shared SPI controller.
#[must_use]
pub fn shared_spi_device(
    manager: &'static SpiBusManager,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> SharedSpiDevice {
    SharedSpiDevice {
        manager,
        chip_select: digital_output(chip_select, DigitalLevel::High),
        config: SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
        frequency_hz,
        id: manager.register(),
    }
}

impl SharedSpiDevice {
    /// Runs `transfer` with the bus configured for this device, selecting
    /// the device around it when `select` is set.
    fn with_bus(
        &mut self,
        select: bool,
        transfer: impl FnOnce(&mut SpiBus) -> Result<(), esp_hal::spi::Error>,
    ) -> Result<(), SharedSpiError> {
        let (mut bus, reconfigure) = self.manager.take(self.id).ok_or(SharedSpiError::Busy)?;
        if reconfigure && bus.apply_config(&self.config).is_err() {
            self.manager.restore(bus, 0);
            return Err(SharedSpiError::Configuration);
        }
        if select {
            self.chip_select.set_low();
        }
        let result = transfer(&mut bus).and_then(|()| embedded_hal::spi::SpiBus::flush(&mut bus));
        if select {
            self.chip_select.set_high();
        }
        self.manager.restore(bus, self.id);
        result.map_err(SharedSpiError::Bus)
    }
}

impl embedded_hal::spi::ErrorType for SharedSpiDevice {
    type Error = SharedSpiError;
}

impl embedded_hal::spi::SpiDevice for SharedSpiDevice {
    fn transaction(
        &mut self,
        operations: &mut [embedded_hal::spi::Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        self.with_bus(true, |bus| {
            for operation in operations {
                match operation {
                    embedded_hal::spi::Operation::Read(words) => {
                        embedded_hal::spi::SpiBus::read(bus, words)?;
                    }
                    embedded_hal::spi::Operation::Write(words) => {
                        embedded_hal::spi::SpiBus::write(bus, words)?;
                    }
                    embedded_hal::spi::Operation::Transfer(read, write) => {
                        embedded_hal::spi::SpiBus::transfer(bus, read, write)?;
                    }
                    embedded_hal::spi::Operation::TransferInPlace(words) => {
                        embedded_hal::spi::SpiBus::transfer_in_place(bus, words)?;
                    }
                    embedded_hal::spi::Operation::DelayNs(nanoseconds) => {
                        embedded_hal::spi::SpiBus::flush(bus)?;
                        embedded_hal::delay::DelayNs::delay_ns(&mut Delay::new(), *nanoseconds);
                    }
                }
            }
            Ok(())
        })
    }
}

impl SpiDeviceControl for SharedSpiDevice {
    fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }

    fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error> {
        self.config = self.config.with_frequency(Rate::from_hz(frequency_hz));
        self.frequency_hz = frequency_hz;
        self.manager.forget_configuration(self.id);
        Ok(())
    }

    fn write_deselected(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.with_bus(false, |bus| embedded_hal::spi::SpiBus::write(bus, bytes))
    }
}

/// Calibration the Board's analog inputs apply: the chip's eFuse
/// line fit, which every ESP chip this file serves has.
type AnalogCalibration<ADC> = AdcCalLine<ADC>;

/// Static owner of one ADC controller whose channels several peripheral
/// implementations sample. A conversion takes tens of microseconds, inside
/// a critical section.
pub struct AnalogConverter<ADC: 'static> {
    adc: CriticalSectionMutex<RefCell<Adc<'static, ADC, Blocking>>>,
}

impl<ADC: 'static> AnalogConverter<ADC> {
    /// Wraps one configured converter for channel-level sharing.
    #[must_use]
    pub const fn new(adc: Adc<'static, ADC, Blocking>) -> Self {
        Self {
            adc: CriticalSectionMutex::new(RefCell::new(adc)),
        }
    }
}

/// Starts the configuration of one ADC controller's Board channels.
#[must_use]
pub fn analog_config<ADC>() -> AdcConfig<ADC> {
    AdcConfig::new()
}

/// Enables one Board analog input on its controller at 11 dB attenuation,
/// the range (to about 3.1 V) that resistor dividers and button ladders on
/// these Boards are built for.
pub fn analog_channel<ADC, P>(
    config: &mut AdcConfig<ADC>,
    pin: P,
) -> AdcPin<P, ADC, AnalogCalibration<ADC>>
where
    P: AdcChannel + AnalogPin,
    AnalogCalibration<ADC>: AdcCalScheme<ADC>,
{
    config.enable_pin_with_cal(pin, Attenuation::_11dB)
}

/// Builds one shared ADC controller from its Board channels' configuration
/// and allocates its owner statically at the Board call site.
#[macro_export]
macro_rules! __barracuda_esp_analog_converter {
    ($adc:ident, $controller:expr, $config:expr) => {{
        static CONVERTER: $crate::hal::__StaticCell<
            $crate::hal::AnalogConverter<$crate::hal::__vendor::peripherals::$adc<'static>>,
        > = $crate::hal::__StaticCell::new();
        let converter: &'static $crate::hal::AnalogConverter<
            $crate::hal::__vendor::peripherals::$adc<'static>,
        > = CONVERTER.init($crate::hal::AnalogConverter::new(
            $crate::hal::__vendor::analog::adc::Adc::new($controller, $config),
        ));
        converter
    }};
}

#[doc(hidden)]
pub use __barracuda_esp_analog_converter as analog_converter;

/// One Board analog input, sampled in calibrated millivolts.
pub struct AnalogInputChannel<ADC: 'static, P> {
    converter: &'static AnalogConverter<ADC>,
    pin: AdcPin<P, ADC, AnalogCalibration<ADC>>,
}

/// Hands one enabled channel of a shared converter to its implementation.
#[must_use]
pub fn analog_input<ADC, P>(
    converter: &'static AnalogConverter<ADC>,
    pin: AdcPin<P, ADC, AnalogCalibration<ADC>>,
) -> AnalogInputChannel<ADC, P> {
    AnalogInputChannel { converter, pin }
}

impl<ADC, P> AnalogErrorType for AnalogInputChannel<ADC, P> {
    type Error = ExposedAnalogError;
}

impl<ADC, P> AnalogVoltageInput for AnalogInputChannel<ADC, P>
where
    ADC: AdcRegisterAccess + 'static,
    P: AdcChannel,
    AnalogCalibration<ADC>: AdcCalScheme<ADC>,
{
    fn read_millivolts(&mut self) -> Result<u32, Self::Error> {
        let pin = &mut self.pin;
        self.converter.adc.lock(|adc| {
            let mut adc = adc.borrow_mut();
            loop {
                match adc.read_oneshot(pin) {
                    Ok(millivolts) => return Ok(u32::from(millivolts)),
                    Err(nb::Error::WouldBlock) => {}
                    Err(nb::Error::Other(())) => return Err(ExposedAnalogError::Conversion),
                }
            }
        })
    }
}
/// Single-owner SPI device whose one data line carries writes and reads.
pub type ThreeWireSpiDevice = ExclusiveDevice<ThreeWireSpiBus, DigitalOutput, Delay>;

/// Blocking SPI bus with one bidirectional data line (three-wire SPI).
///
/// Writes drive the line as MOSI. Reads release it and sample the device's
/// answer on the same line through the controller's half-duplex SIO mode, so
/// a device such as a display controller can be identified by reading its ID
/// registers. Full-duplex transfers cannot exist on one line and are rejected.
pub struct ThreeWireSpiBus {
    spi: SpiBus,
    write_config: SpiConfig,
    read_config: SpiConfig,
}

impl embedded_hal::spi::ErrorType for ThreeWireSpiBus {
    type Error = esp_hal::spi::Error;
}

impl embedded_hal::spi::SpiBus for ThreeWireSpiBus {
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        if words.is_empty() {
            return Ok(());
        }
        // Both clocks were validated when the device was constructed.
        self.spi
            .apply_config(&self.read_config)
            .map_err(|_| esp_hal::spi::Error::Unsupported)?;
        let result = self.spi.half_duplex_read(
            esp_hal::spi::master::DataMode::Single,
            esp_hal::spi::master::Command::None,
            esp_hal::spi::master::Address::None,
            0,
            words,
        );
        self.spi
            .apply_config(&self.write_config)
            .map_err(|_| esp_hal::spi::Error::Unsupported)?;
        result
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::write(&mut self.spi, words)
    }

    fn transfer(&mut self, _read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
        Err(esp_hal::spi::Error::Unsupported)
    }

    fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        Err(esp_hal::spi::Error::Unsupported)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::flush(&mut self.spi)
    }
}
/// Blocking I2C controller passed to a peripheral implementation.
pub type I2cBus = I2c<'static, Blocking>;
/// Static owner of one I2C bus shared by peripheral implementations.
pub type I2cBusManager = SharedI2cBus<I2cBus>;
/// Blocking I2C view handed to one peripheral implementation.
pub type I2cDevice = embedded_hal_bus::i2c::CriticalSectionDevice<'static, I2cBus>;

/// Statically allocates one shared built-in I2C bus owner at the Board call site.
#[macro_export]
macro_rules! __barracuda_esp_i2c_bus_manager {
    ($bus:expr) => {{
        static MANAGER: $crate::hal::__StaticCell<$crate::hal::I2cBusManager> =
            $crate::hal::__StaticCell::new();
        MANAGER.init($crate::hal::I2cBusManager::new($bus))
    }};
}

#[doc(hidden)]
pub use __barracuda_esp_i2c_bus_manager as i2c_bus_manager;
/// Send-safe I2C adapter exposed through the async ecosystem contract.
///
/// ESP HAL's interrupt-backed async marker is intentionally `!Send`, while a
/// Barracuda Plugin handle can move between executor tasks. Runtime buses use
/// the blocking peripheral driver behind async trait methods so ownership can
/// remain safe without an unsafe `Send` assertion.
pub struct ExposedI2cBus(I2c<'static, Blocking>);

impl embedded_hal::i2c::ErrorType for ExposedI2cBus {
    type Error = esp_hal::i2c::master::Error;
}

impl embedded_hal_async::i2c::I2c for ExposedI2cBus {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        embedded_hal::i2c::I2c::transaction(&mut self.0, address, operations)
    }
}

/// Send-safe SPI adapter exposed through the async ecosystem contract.
pub struct ExposedSpiBus(Spi<'static, Blocking>);

impl embedded_hal::spi::ErrorType for ExposedSpiBus {
    type Error = esp_hal::spi::Error;
}

impl embedded_hal_async::spi::SpiBus for ExposedSpiBus {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::read(&mut self.0, words)
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::write(&mut self.0, words)
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer(&mut self.0, read, write)
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer_in_place(&mut self.0, words)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::flush(&mut self.0)
    }
}

/// Send-safe UART adapter exposed through the async byte-stream contract.
pub struct ExposedUart(Uart<'static, Blocking>);

/// ESP LEDC channel exposed through the standard PWM contract.
#[cfg(feature = "runtime-pwm")]
pub type ExposedPwm = Channel<'static, LowSpeed>;
/// Uninhabited PWM output on ESP targets without a vendor LEDC driver.
#[cfg(not(feature = "runtime-pwm"))]
pub type ExposedPwm = UnavailablePwm;

/// Statically allocated timer descriptor generated from a Platform manifest.
#[cfg(feature = "runtime-pwm")]
#[derive(Clone, Copy)]
pub struct RuntimePwmTimer {
    number: LedcTimerNumber,
    storage: &'static __StaticCell<Timer<'static, LowSpeed>>,
}

#[cfg(feature = "runtime-pwm")]
impl RuntimePwmTimer {
    /// Creates one move-only runtime timer descriptor.
    #[must_use]
    pub const fn new(
        number: LedcTimerNumber,
        storage: &'static __StaticCell<Timer<'static, LowSpeed>>,
    ) -> Self {
        Self { number, storage }
    }
}

/// Platform-owned LEDC controller, timer, and channel allocation.
#[cfg(feature = "runtime-pwm")]
pub struct RuntimePwmResource {
    controller: PwmController,
    timer: Option<RuntimePwmTimer>,
    channel: Option<LedcChannelNumber>,
}

type AdcFactory = fn(
    esp_hal::peripherals::ADC1<'static>,
    AnyPin<'static>,
) -> Result<ExposedAnalogInput, ExposedAnalogError>;

/// One typed ADC channel route declared by the Platform manifest.
#[derive(Clone, Copy)]
pub struct RuntimeAdcChannel {
    pin_number: u8,
    factory: AdcFactory,
}

/// Platform-owned ADC1 controller and its statically valid channel routes.
pub struct RuntimeAdcResource {
    controller: esp_hal::peripherals::ADC1<'static>,
    channels: [Option<RuntimeAdcChannel>; 10],
}

/// Failure while constructing or sampling an exposed ESP ADC input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposedAnalogError {
    /// The selected exposed pin does not match the statically declared channel.
    InvalidPin,
    /// The ADC reported a conversion failure.
    Conversion,
}

impl core::fmt::Display for ExposedAnalogError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidPin => formatter.write_str("invalid ESP ADC channel route"),
            Self::Conversion => formatter.write_str("ESP ADC conversion failed"),
        }
    }
}

impl core::error::Error for ExposedAnalogError {}

/// Type-erased, typed-channel ADC input handed to a synchronous consumer.
pub struct ExposedAnalogInput(Box<dyn AnalogInput<Error = ExposedAnalogError>>);

impl AnalogErrorType for ExposedAnalogInput {
    type Error = ExposedAnalogError;
}

impl AnalogInput for ExposedAnalogInput {
    fn max_value(&self) -> u32 {
        self.0.max_value()
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        self.0.read()
    }
}

struct TypedAdcInput<P> {
    adc: Adc<'static, esp_hal::peripherals::ADC1<'static>, Blocking>,
    pin: AdcPin<P, esp_hal::peripherals::ADC1<'static>>,
}

impl<P> AnalogErrorType for TypedAdcInput<P> {
    type Error = ExposedAnalogError;
}

impl<P> AnalogInput for TypedAdcInput<P>
where
    P: AdcChannel,
{
    fn max_value(&self) -> u32 {
        4095
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        loop {
            match self.adc.read_oneshot(&mut self.pin) {
                Ok(value) => return Ok(u32::from(value)),
                Err(nb::Error::WouldBlock) => {}
                Err(nb::Error::Other(())) => return Err(ExposedAnalogError::Conversion),
            }
        }
    }
}

/// Failure while constructing an exposed ESP LEDC output.
#[cfg(feature = "runtime-pwm")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExposedPwmError {
    /// The manifest did not provide a timer and channel pair.
    MissingResource,
    /// None of the supported duty resolutions can produce the requested frequency.
    Timer(LedcTimerError),
    /// The LEDC channel rejected its timer or output configuration.
    Channel(LedcChannelError),
}

#[cfg(feature = "runtime-pwm")]
impl core::fmt::Display for ExposedPwmError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingResource => formatter.write_str("missing ESP LEDC timer or channel"),
            Self::Timer(_) => formatter.write_str("invalid ESP LEDC timer configuration"),
            Self::Channel(_) => formatter.write_str("invalid ESP LEDC channel configuration"),
        }
    }
}

#[cfg(feature = "runtime-pwm")]
impl core::error::Error for ExposedPwmError {}

impl embedded_io::ErrorType for ExposedUart {
    type Error = esp_hal::uart::IoError;
}

impl embedded_io_async::Read for ExposedUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io::Read::read(&mut self.0, buffer)
    }
}

impl embedded_io_async::Write for ExposedUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        embedded_io::Write::write(&mut self.0, buffer)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io::Write::flush(&mut self.0)
    }
}
/// Delay provider used during peripheral initialization.
pub type PeripheralDelay = Delay;
/// SPI configuration failure surfaced by generated Board initialization.
pub type SpiConfigError = SpiConfigErrorInner;
/// I2C configuration failure surfaced by generated Board initialization.
pub type I2cConfigError = I2cConfigErrorInner;

/// Failure while configuring a runtime ESP UART.
#[derive(Debug)]
pub enum ExposedUartConfigError {
    /// The ESP HAL rejected the requested line configuration.
    Peripheral(UartConfigError),
    /// ESP UART hardware does not support nine data bits.
    UnsupportedDataBits,
}

impl core::fmt::Display for ExposedUartConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Peripheral(error) => error.fmt(formatter),
            Self::UnsupportedDataBits => formatter.write_str("ESP UART supports 7 or 8 data bits"),
        }
    }
}

impl core::error::Error for ExposedUartConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Peripheral(error) => Some(error),
            Self::UnsupportedDataBits => None,
        }
    }
}

/// Type-erased Platform adapter used by generated runtime I/O composition.
pub struct RuntimeAdapter;

impl RuntimePlatform for RuntimeAdapter {
    type PinToken = AnyPin<'static>;
    type DigitalPin = DynamicPin;
    type I2cController = AnyI2c<'static>;
    type I2cBus = ExposedI2cBus;
    type I2cError = I2cConfigError;
    type SpiController = AnySpi<'static>;
    type SpiBus = ExposedSpiBus;
    type SpiError = SpiConfigError;
    type UartController = AnyUart<'static>;
    type AdcResource = RuntimeAdcResource;
    type PwmResource = PlatformPwmResource;
    type I2sResource = PlatformI2sResource;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        DynamicPin {
            pin: Flex::new(pin),
        }
    }

    fn supports_i2c_config(
        _controller: &Self::I2cController,
        _scl: &Self::PinToken,
        _sda: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0
    }

    fn i2c(
        controller: Self::I2cController,
        scl: Self::PinToken,
        sda: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError> {
        exposed_i2c(controller, scl, sda, frequency_hz)
    }

    fn supports_spi_config(
        _controller: &Self::SpiController,
        _sck: &Self::PinToken,
        mosi: Option<&Self::PinToken>,
        miso: Option<&Self::PinToken>,
        frequency_hz: u32,
        _mode: EmbeddedMode,
    ) -> bool {
        frequency_hz > 0 && (mosi.is_some() || miso.is_some())
    }

    fn spi(
        controller: Self::SpiController,
        sck: Self::PinToken,
        mosi: Option<Self::PinToken>,
        miso: Option<Self::PinToken>,
        frequency_hz: u32,
        mode: EmbeddedMode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        let mode = hal_spi_mode(mode);
        let bus = Spi::new(
            controller,
            SpiConfig::default()
                .with_frequency(Rate::from_hz(frequency_hz))
                .with_mode(mode),
        )?
        .with_sck(sck);
        let bus = if let Some(mosi) = mosi {
            bus.with_mosi(mosi)
        } else {
            bus
        };
        let bus = if let Some(miso) = miso {
            bus.with_miso(miso)
        } else {
            bus
        };
        Ok(ExposedSpiBus(bus))
    }
}

impl RuntimeAnalogPlatform for RuntimeAdapter {
    type AnalogInput = ExposedAnalogInput;
    type AnalogOutput = UnavailableAnalogOutput;
    type AnalogError = ExposedAnalogError;

    fn supports_analog_input(resource: &Self::AdcResource, pin: &Self::PinToken) -> bool {
        resource
            .channels
            .iter()
            .flatten()
            .any(|channel| channel.pin_number == pin.number())
    }

    fn supports_analog_output(_pin: &Self::PinToken) -> bool {
        false
    }

    fn analog_input(
        resource: Self::AdcResource,
        pin: Self::PinToken,
    ) -> Result<Self::AnalogInput, Self::AnalogError> {
        let factory = resource
            .channels
            .iter()
            .flatten()
            .find(|channel| channel.pin_number == pin.number())
            .map(|channel| channel.factory)
            .ok_or(ExposedAnalogError::InvalidPin)?;
        factory(resource.controller, pin)
    }

    fn analog_output(_pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Err(ExposedAnalogError::InvalidPin)
    }
}

#[cfg(feature = "runtime-pwm")]
impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = ExposedPwm;
    type PwmError = ExposedPwmError;

    fn supports_pwm(resource: &Self::PwmResource, _pin: &Self::PinToken) -> bool {
        resource.timer.is_some() && resource.channel.is_some()
    }

    fn supports_pwm_config(
        resource: &Self::PwmResource,
        pin: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0 && Self::supports_pwm(resource, pin)
    }

    fn pwm(
        mut resource: Self::PwmResource,
        pin: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        let timer = resource
            .timer
            .take()
            .ok_or(ExposedPwmError::MissingResource)?;
        let channel_number = resource
            .channel
            .take()
            .ok_or(ExposedPwmError::MissingResource)?;
        let ledc = resource.controller;
        let mut configured_timer = ledc.timer(timer.number);
        configure_ledc_timer(&mut configured_timer, frequency_hz)?;
        let configured_timer = timer.storage.init(configured_timer);
        let mut channel = ledc.channel(channel_number, pin);
        channel
            .configure(esp_hal::ledc::channel::config::Config {
                timer: configured_timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .map_err(ExposedPwmError::Channel)?;
        Ok(channel)
    }
}

#[cfg(not(feature = "runtime-pwm"))]
impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = ExposedPwm;
    type PwmError = core::convert::Infallible;

    fn supports_pwm(_resource: &Self::PwmResource, _pin: &Self::PinToken) -> bool {
        false
    }

    fn supports_pwm_config(
        _resource: &Self::PwmResource,
        _pin: &Self::PinToken,
        _frequency_hz: u32,
    ) -> bool {
        false
    }

    fn pwm(
        resource: Self::PwmResource,
        _pin: Self::PinToken,
        _frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        match resource {}
    }
}

#[cfg(feature = "runtime-pwm")]
fn configure_ledc_timer(
    timer: &mut Timer<'static, LowSpeed>,
    frequency_hz: u32,
) -> Result<(), ExposedPwmError> {
    let duties = [
        LedcDuty::Duty14Bit,
        LedcDuty::Duty13Bit,
        LedcDuty::Duty12Bit,
        LedcDuty::Duty11Bit,
        LedcDuty::Duty10Bit,
        LedcDuty::Duty9Bit,
        LedcDuty::Duty8Bit,
        LedcDuty::Duty7Bit,
        LedcDuty::Duty6Bit,
        LedcDuty::Duty5Bit,
        LedcDuty::Duty4Bit,
        LedcDuty::Duty3Bit,
        LedcDuty::Duty2Bit,
        LedcDuty::Duty1Bit,
    ];
    let mut last_error = LedcTimerError::Divisor;
    for duty in duties {
        match timer.configure(LedcTimerConfig {
            duty,
            clock_source: LSClockSource::APBClk,
            frequency: Rate::from_hz(frequency_hz),
        }) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(ExposedPwmError::Timer(last_error))
}

impl RuntimeUartPlatform for RuntimeAdapter {
    type Uart = ExposedUart;
    type UartError = ExposedUartConfigError;

    fn supports_uart(
        _controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
    ) -> bool {
        tx.is_some() || rx.is_some()
    }

    fn supports_uart_config(
        controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
        config: UartConfig,
    ) -> bool {
        config.baud > 0 && Self::supports_uart(controller, tx, rx)
    }

    fn uart(
        controller: Self::UartController,
        tx: Option<Self::PinToken>,
        rx: Option<Self::PinToken>,
        config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError> {
        let data_bits = match config.data_bits {
            UartDataBits::Seven => DataBits::_7,
            UartDataBits::Eight => DataBits::_8,
            UartDataBits::Nine => return Err(ExposedUartConfigError::UnsupportedDataBits),
        };
        let parity = match config.parity {
            UartParity::None => Parity::None,
            UartParity::Even => Parity::Even,
            UartParity::Odd => Parity::Odd,
        };
        let stop_bits = match config.stop_bits {
            UartStopBits::One => StopBits::_1,
            UartStopBits::Two => StopBits::_2,
        };
        let config = HalUartConfig::default()
            .with_baudrate(config.baud)
            .with_data_bits(data_bits)
            .with_parity(parity)
            .with_stop_bits(stop_bits);
        let uart = Uart::new(controller, config).map_err(ExposedUartConfigError::Peripheral)?;
        let uart = if let Some(tx) = tx {
            uart.with_tx(tx)
        } else {
            uart
        };
        let uart = if let Some(rx) = rx {
            uart.with_rx(rx)
        } else {
            uart
        };
        Ok(ExposedUart(uart))
    }
}

#[cfg(not(feature = "runtime-i2s"))]
impl ::barracuda_board_hal::RuntimeI2sPlatform for RuntimeAdapter {
    type I2s = ::barracuda_board_hal::UnavailableI2s;
    type I2sError = ::barracuda_board_hal::UnsupportedFunction;

    fn supports_i2s(
        _resource: &Self::I2sResource,
        _bclk: &Self::PinToken,
        _ws: &Self::PinToken,
        _dout: Option<&Self::PinToken>,
        _din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        false
    }

    fn i2s(
        resource: Self::I2sResource,
        _bclk: Self::PinToken,
        _ws: Self::PinToken,
        _dout: Option<Self::PinToken>,
        _din: Option<Self::PinToken>,
        _mclk: Option<Self::PinToken>,
        _format: ::barracuda_board_hal::audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        match resource {}
    }
}

/// Generated exposed-I/O owner specialized to the ESP Platform adapter.
pub type RuntimeIo<
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
> = barracuda_board_hal::RuntimeIo<RuntimeAdapter, P, I, S, U, A, W, T>;

/// Constructs the selected Board's unified runtime I/O owner.
#[must_use]
pub fn runtime_io<
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
>(
    pins: [(&'static str, AnyPin<'static>); P],
    i2c: [AnyI2c<'static>; I],
    spi: [AnySpi<'static>; S],
    uart: [AnyUart<'static>; U],
    adc: [RuntimeAdcResource; A],
    pwm: [PlatformPwmResource; W],
    i2s: [PlatformI2sResource; T],
) -> RuntimeIo<P, I, S, U, A, W, T> {
    RuntimeIo::new_with_resources(pins, i2c, spi, uart, adc, pwm, i2s)
}

/// Erases one selected pin token while preserving its exclusive ownership.
#[must_use]
pub fn runtime_pin(pin: impl Pin + 'static) -> AnyPin<'static> {
    pin.degrade()
}

/// Erases one selected I2C controller token for the runtime pool.
#[must_use]
pub fn runtime_i2c_controller(i2c: impl I2cInstance + 'static) -> AnyI2c<'static> {
    i2c.degrade()
}

/// Erases one selected SPI controller token for the runtime pool.
#[must_use]
pub fn runtime_spi_controller(spi: impl SpiInstance + 'static) -> AnySpi<'static> {
    spi.degrade()
}

/// Erases one selected UART controller token for the runtime pool.
#[must_use]
pub fn runtime_uart_controller(uart: impl UartInstance + 'static) -> AnyUart<'static> {
    uart.degrade()
}

/// Builds one Platform-owned ESP ADC1 allocation from generated channel routes.
#[must_use]
pub fn runtime_adc_resource(
    controller: esp_hal::peripherals::ADC1<'static>,
    channels: &[RuntimeAdcChannel],
) -> RuntimeAdcResource {
    let mut declared = [None; 10];
    for (target, channel) in declared.iter_mut().zip(channels.iter().copied()) {
        *target = Some(channel);
    }
    RuntimeAdcResource {
        controller,
        channels: declared,
    }
}

/// Creates one typed ADC route without erasing the vendor pin/channel identity.
#[must_use]
pub fn runtime_adc_channel_for<P>(pin_number: u8) -> RuntimeAdcChannel
where
    P: Pin + AnalogPin + AdcChannel + 'static,
    AnyPin<'static>: TryInto<P, Error = AnyPin<'static>>,
{
    RuntimeAdcChannel {
        pin_number,
        factory: construct_adc_input::<P>,
    }
}

fn construct_adc_input<P>(
    controller: esp_hal::peripherals::ADC1<'static>,
    pin: AnyPin<'static>,
) -> Result<ExposedAnalogInput, ExposedAnalogError>
where
    P: Pin + AnalogPin + AdcChannel + 'static,
    AnyPin<'static>: TryInto<P, Error = AnyPin<'static>>,
{
    let pin = pin
        .downcast::<P>()
        .map_err(|_| ExposedAnalogError::InvalidPin)?;
    let mut config = AdcConfig::new();
    let pin = config.enable_pin(pin, Attenuation::_11dB);
    let adc = Adc::new(controller, config);
    Ok(ExposedAnalogInput(Box::new(TypedAdcInput { adc, pin })))
}

/// Resolves the typed ADC channel and pin pair from a Platform manifest.
#[macro_export]
macro_rules! __barracuda_esp32_runtime_adc_channel {
    ($channel:ident, $pin:ident) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::$pin<'static>>(
            $crate::hal::runtime_gpio_number!($pin),
        )
    };
}

/// Resolves an ESP GPIO singleton name to its hardware number.
#[macro_export]
macro_rules! __barracuda_esp32_runtime_gpio_number {
    (GPIO0) => {
        0
    };
    (GPIO1) => {
        1
    };
    (GPIO2) => {
        2
    };
    (GPIO3) => {
        3
    };
    (GPIO4) => {
        4
    };
    (GPIO5) => {
        5
    };
    (GPIO6) => {
        6
    };
    (GPIO7) => {
        7
    };
    (GPIO8) => {
        8
    };
    (GPIO9) => {
        9
    };
    (GPIO10) => {
        10
    };
    (GPIO16) => {
        16
    };
    (GPIO17) => {
        17
    };
    (GPIO18) => {
        18
    };
    (GPIO19) => {
        19
    };
    (GPIO20) => {
        20
    };
    (GPIO21) => {
        21
    };
    (GPIO22) => {
        22
    };
    (GPIO23) => {
        23
    };
    (GPIO32) => {
        32
    };
    (GPIO33) => {
        33
    };
    (GPIO34) => {
        34
    };
    (GPIO35) => {
        35
    };
    (GPIO36) => {
        36
    };
    (GPIO37) => {
        37
    };
    (GPIO38) => {
        38
    };
    (GPIO39) => {
        39
    };
}

#[doc(hidden)]
pub use __barracuda_esp32_runtime_adc_channel as runtime_adc_channel;
#[doc(hidden)]
pub use __barracuda_esp32_runtime_gpio_number as runtime_gpio_number;

/// Builds one Platform-owned ESP LEDC allocation from generated descriptors.
#[must_use]
#[cfg(feature = "runtime-pwm")]
pub fn runtime_pwm_resource(
    controller: PwmController,
    timers: &[RuntimePwmTimer],
    channels: &[LedcChannelNumber],
) -> RuntimePwmResource {
    RuntimePwmResource {
        controller,
        timer: timers.first().copied(),
        channel: channels.first().copied(),
    }
}

/// Resolves one manifest timer name and allocates its static vendor storage.
#[macro_export]
#[cfg(feature = "runtime-pwm")]
macro_rules! __barracuda_esp32_runtime_pwm_timer {
    ($timer:ident) => {{
        static STORAGE: $crate::hal::__StaticCell<
            $crate::hal::__vendor::ledc::timer::Timer<
                'static,
                $crate::hal::__vendor::ledc::LowSpeed,
            >,
        > = $crate::hal::__StaticCell::new();
        $crate::hal::RuntimePwmTimer::new(
            $crate::hal::__vendor::ledc::timer::Number::$timer,
            &STORAGE,
        )
    }};
}

/// Resolves one manifest LEDC channel name.
#[macro_export]
#[cfg(feature = "runtime-pwm")]
macro_rules! __barracuda_esp32_runtime_pwm_channel {
    ($channel:ident) => {
        $crate::hal::__vendor::ledc::channel::Number::$channel
    };
}

#[doc(hidden)]
#[cfg(feature = "runtime-pwm")]
pub use __barracuda_esp32_runtime_pwm_channel as runtime_pwm_channel;
#[doc(hidden)]
#[cfg(feature = "runtime-pwm")]
pub use __barracuda_esp32_runtime_pwm_timer as runtime_pwm_timer;

/// Converts a selected raw pin token into a digital output.
#[must_use]
pub fn digital_output(pin: impl OutputPin + 'static, initial: DigitalLevel) -> DigitalOutput {
    let level = match initial {
        DigitalLevel::Low => Level::Low,
        DigitalLevel::High => Level::High,
    };
    Output::new(pin, level, HalOutputConfig::default())
}

/// Converts a selected raw pin token into a digital input.
#[must_use]
pub fn digital_input(pin: impl InputPin + 'static) -> DigitalInput {
    Input::new(pin, HalInputConfig::default())
}

/// A pin a peripheral implementation switches between input and output.
pub type ConfigurablePin = DynamicPin;

/// Converts a selected raw pin token into a pin its implementation
/// configures. Its output stays disabled until the implementation configures
/// one.
#[must_use]
pub fn configurable_pin(pin: impl Pin + 'static) -> ConfigurablePin {
    DynamicPin {
        pin: Flex::new(pin),
    }
}

/// Runtime-configurable ESP32 GPIO exposed through `embedded-hal`.
pub struct DynamicPin {
    pin: Flex<'static>,
}

impl ErrorType for DynamicPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::InputPin for DynamicPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        embedded_hal::digital::InputPin::is_high(&mut self.pin)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        embedded_hal::digital::InputPin::is_low(&mut self.pin)
    }
}

impl embedded_hal::digital::OutputPin for DynamicPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        embedded_hal::digital::OutputPin::set_low(&mut self.pin)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        embedded_hal::digital::OutputPin::set_high(&mut self.pin)
    }
}

impl StatefulOutputPin for DynamicPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        StatefulOutputPin::is_set_high(&mut self.pin)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        StatefulOutputPin::is_set_low(&mut self.pin)
    }
}

impl ConfigurableDigitalPin for DynamicPin {
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
        let pull = match config.pull {
            BoardPull::None => Pull::None,
            BoardPull::Up => Pull::Up,
            BoardPull::Down => Pull::Down,
        };
        self.pin.set_output_enable(false);
        self.pin
            .apply_input_config(&HalInputConfig::default().with_pull(pull));
        self.pin.set_input_enable(true);
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        match config.initial {
            DigitalLevel::Low => embedded_hal::digital::OutputPin::set_low(&mut self.pin)?,
            DigitalLevel::High => embedded_hal::digital::OutputPin::set_high(&mut self.pin)?,
        }
        let output = match config.drive {
            OutputDrive::PushPull => HalOutputConfig::default(),
            OutputDrive::OpenDrain => {
                HalOutputConfig::default().with_drive_mode(DriveMode::OpenDrain)
            }
        };
        self.pin.apply_output_config(&output);
        self.pin
            .set_input_enable(config.drive == OutputDrive::OpenDrain);
        self.pin.set_output_enable(true);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.pin.set_output_enable(false);
        self.pin.set_input_enable(false);
        Ok(())
    }
}

/// Constructs one SPI device from Board-selected controller and pins.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi);
    Ok(exclusive_spi_device(bus, chip_select, frequency_hz))
}

/// Constructs one transmit-only SPI bus from Board-selected resources.
pub fn spi_bus(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi);
    Ok(SpiBus(bus))
}

/// Constructs one data-only SPI waveform output from a Platform-owned controller.
pub fn spi_output(
    spi: impl SpiInstance + 'static,
    data: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_mosi(data);
    Ok(SpiBus(bus))
}

/// Constructs one receive-only SPI bus from Board-selected resources.
pub fn spi_bus_rx_only(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_miso(miso);
    Ok(SpiBus(bus))
}

/// Constructs one full-duplex SPI bus from Board-selected resources.
pub fn spi_bus_full_duplex(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso);
    Ok(SpiBus(bus))
}

/// Constructs one receive-only SPI device from Board-selected resources.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device_rx_only(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_miso(miso);
    Ok(exclusive_spi_device(bus, chip_select, frequency_hz))
}

/// Constructs one full-duplex SPI device from Board-selected resources.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device_full_duplex(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso);
    Ok(exclusive_spi_device(bus, chip_select, frequency_hz))
}

/// Constructs one three-wire SPI device whose `data` line carries both
/// directions; reads run at `read_frequency_hz`.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when either requested frequency is unsupported.
pub fn spi_device_three_wire(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    data: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
    read_frequency_hz: u32,
) -> Result<ThreeWireSpiDevice, SpiConfigError> {
    let write_config = SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz));
    let read_config = SpiConfig::default().with_frequency(Rate::from_hz(read_frequency_hz));
    let mut spi = Spi::new(spi, read_config)?.with_sck(sck).with_sio0(data);
    spi.apply_config(&write_config)?;
    let chip_select = digital_output(chip_select, DigitalLevel::High);
    let bus = ThreeWireSpiBus {
        spi: SpiBus(spi),
        write_config,
        read_config,
    };
    match ExclusiveDevice::new(bus, chip_select, Delay::new()) {
        Ok(device) => Ok(device),
        Err(never) => match never {},
    }
}

fn exclusive_spi_device(
    bus: Spi<'static, Blocking>,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> SpiDevice {
    let chip_select = digital_output(chip_select, DigitalLevel::High);
    match ExclusiveDevice::new(SpiBus(bus), chip_select, Delay::new()) {
        Ok(device) => SpiDevice {
            device,
            config: SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
            frequency_hz,
        },
        Err(never) => match never {},
    }
}

/// Constructs an I2C bus from Board-selected controller and pins.
///
/// # Errors
///
/// Returns [`I2cConfigError`] when the requested frequency is unsupported.
pub fn i2c_device(
    i2c: impl I2cInstance + 'static,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<I2cBus, I2cConfigError> {
    Ok(I2c::new(
        i2c,
        I2cConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_scl(scl)
    .with_sda(sda))
}

/// Constructs an async I2C controller explicitly exposed by the Board.
pub fn exposed_i2c(
    i2c: impl I2cInstance + 'static,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<ExposedI2cBus, I2cConfigError> {
    Ok(ExposedI2cBus(i2c_device(i2c, scl, sda, frequency_hz)?))
}

/// Constructs the delay provider used by synchronous peripheral implementations.
#[must_use]
pub fn delay() -> PeripheralDelay {
    Delay::new()
}

/// Resolves a Board-selected pin name to its move-only ESP HAL token type.
#[macro_export]
macro_rules! __barracuda_esp32_pin_binding_type {
    ($pin:ident) => {
        $crate::hal::__vendor::peripherals::$pin<'static>
    };
}

/// Resolves a Board-selected controller name to its move-only ESP HAL token type.
#[macro_export]
macro_rules! __barracuda_esp32_controller_binding_type {
    ($peripheral:ident) => {
        $crate::hal::__vendor::peripherals::$peripheral<'static>
    };
}

#[doc(hidden)]
pub use __barracuda_esp32_controller_binding_type as controller_binding_type;
#[doc(hidden)]
pub use __barracuda_esp32_pin_binding_type as pin_binding_type;

/// Counter clock of the remote-control transceiver: the 80 MHz default
/// source on every ESP chip this file serves (the ESP32 accepts no other).
const RMT_CLOCK_HZ: u32 = 80_000_000;

/// Symbols one ESP32 receive records: its channel memory, 64 symbols, which
/// the chip cannot refill during a reception. The other chips refill it.
const RMT_RX_BLOCK_SYMBOLS: usize = 64;

/// How often an awaited transfer checks the hardware. Infrared symbols last
/// half a millisecond or more, so half a channel's memory takes over 10 ms
/// to send or fill, and a check every 500 µs refills or drains it in time.
const RMT_POLL_MICROS: u64 = 500;

/// The Board's remote-control transceiver, whose channels its pulse-train
/// bindings take one by one.
///
/// It runs in the vendor's blocking mode: the channels then move freely
/// between tasks, and an awaited transfer polls the hardware between short
/// timer waits instead of binding the transceiver's interrupt.
pub type RmtController = esp_hal::rmt::Rmt<'static, Blocking>;

/// Failure to build or use a transceiver channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RmtError {
    /// The transceiver rejected a clock, channel, or memory configuration.
    Config(esp_hal::rmt::ConfigError),
    /// A transmission or reception failed.
    Transfer(esp_hal::rmt::Error),
    /// The tick frequency is not the 80 MHz clock divided by 1 to 255.
    Resolution,
    /// The carrier cannot be built from the 80 MHz clock.
    Carrier,
}

impl core::fmt::Display for RmtError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "invalid RMT configuration: {error}"),
            Self::Transfer(error) => write!(formatter, "RMT transfer failed: {error}"),
            Self::Resolution => formatter.write_str("unreachable RMT tick frequency"),
            Self::Carrier => formatter.write_str("unreachable RMT carrier"),
        }
    }
}

impl core::error::Error for RmtError {}

/// Builds the transceiver once for every pulse-train binding of the Board.
///
/// # Errors
///
/// Returns [`RmtError::Config`] when the clock cannot be set.
pub fn rmt_controller(
    rmt: esp_hal::peripherals::RMT<'static>,
) -> Result<RmtController, RmtError> {
    esp_hal::rmt::Rmt::new(rmt, Rate::from_hz(RMT_CLOCK_HZ)).map_err(RmtError::Config)
}

/// Takes one channel of a transceiver built by [`rmt_controller`].
#[macro_export]
macro_rules! __barracuda_esp_rmt_channel {
    ($rmt:ident, 0) => {
        $rmt.channel0
    };
    ($rmt:ident, 1) => {
        $rmt.channel1
    };
    ($rmt:ident, 2) => {
        $rmt.channel2
    };
    ($rmt:ident, 3) => {
        $rmt.channel3
    };
    ($rmt:ident, 4) => {
        $rmt.channel4
    };
    ($rmt:ident, 5) => {
        $rmt.channel5
    };
    ($rmt:ident, 6) => {
        $rmt.channel6
    };
    ($rmt:ident, 7) => {
        $rmt.channel7
    };
}

#[doc(hidden)]
pub use __barracuda_esp_rmt_channel as rmt_channel;

/// Channel divider of the 80 MHz clock for `tick_hz`.
fn rmt_divider(tick_hz: u32) -> Result<u8, RmtError> {
    if tick_hz == 0 || !RMT_CLOCK_HZ.is_multiple_of(tick_hz) {
        return Err(RmtError::Resolution);
    }
    u8::try_from(RMT_CLOCK_HZ / tick_hz)
        .ok()
        .filter(|divider| *divider != 0)
        .ok_or(RmtError::Resolution)
}

const fn rmt_level(high: bool) -> Level {
    if high {
        Level::High
    } else {
        Level::Low
    }
}

/// One transceiver channel that transmits pulse trains on its pin.
///
/// A blocking transfer keeps the CPU while the chain is clocked out, so the
/// channel's memory is refilled without delay, as LED chains need; an awaited
/// transfer polls, as infrared needs.
pub struct PulseOutputChannel {
    channel: esp_hal::rmt::Channel<'static, Blocking, esp_hal::rmt::Tx>,
    config: esp_hal::rmt::TxChannelConfig,
}

/// Configures one transmit channel at 1 MHz ticks, resting low, and routes
/// it to its pin.
///
/// # Errors
///
/// Returns [`RmtError::Config`] when the channel's memory is taken.
pub fn rmt_output(
    channel: impl esp_hal::rmt::TxChannelCreator<'static, Blocking>,
    pin: impl PeripheralOutput<'static>,
) -> Result<PulseOutputChannel, RmtError> {
    let config = esp_hal::rmt::TxChannelConfig::default()
        .with_clk_divider(rmt_divider(1_000_000)?)
        .with_idle_output(true)
        .with_idle_output_level(Level::Low);
    let channel = channel
        .configure_tx(&config)
        .map_err(RmtError::Config)?
        .with_pin(pin);
    Ok(PulseOutputChannel { channel, config })
}

impl barracuda_board_hal::PulseOutput for PulseOutputChannel {
    type Symbol = esp_hal::rmt::PulseCode;
    type Error = RmtError;

    fn symbol(
        first: barracuda_board_hal::PulseLevel,
        second: barracuda_board_hal::PulseLevel,
    ) -> Self::Symbol {
        esp_hal::rmt::PulseCode::new_clamped(
            rmt_level(first.high),
            first.ticks,
            rmt_level(second.high),
            second.ticks,
        )
    }

    fn end_marker() -> Self::Symbol {
        esp_hal::rmt::PulseCode::end_marker()
    }

    fn configure(
        &mut self,
        config: barracuda_board_hal::PulseOutputConfig,
    ) -> Result<(), Self::Error> {
        let (carrier_high, carrier_low) = match config.carrier {
            Some(carrier) => {
                if carrier.frequency_hz == 0 || !(1..=99).contains(&carrier.duty_percent) {
                    return Err(RmtError::Carrier);
                }
                let period = RMT_CLOCK_HZ / carrier.frequency_hz;
                let high = period * u32::from(carrier.duty_percent) / 100;
                let low = period.saturating_sub(high);
                (
                    u16::try_from(high)
                        .ok()
                        .filter(|ticks| *ticks != 0)
                        .ok_or(RmtError::Carrier)?,
                    u16::try_from(low)
                        .ok()
                        .filter(|ticks| *ticks != 0)
                        .ok_or(RmtError::Carrier)?,
                )
            }
            None => (1, 1),
        };
        let updated = self
            .config
            .with_clk_divider(rmt_divider(config.tick_hz)?)
            .with_idle_output_level(rmt_level(config.idle_high))
            .with_carrier_modulation(config.carrier.is_some())
            .with_carrier_high(carrier_high)
            .with_carrier_low(carrier_low)
            // The carrier rides on the level opposite the idle one.
            .with_carrier_level(rmt_level(!config.idle_high));
        self.channel
            .apply_config(&updated)
            .map_err(RmtError::Config)?;
        self.config = updated;
        Ok(())
    }

    fn transmit_blocking(&mut self, symbols: &[Self::Symbol]) -> Result<(), Self::Error> {
        // A reborrowed channel goes back to `self` however the transfer ends.
        self.channel
            .reborrow()
            .transmit(symbols)
            .map_err(|(error, _)| RmtError::Transfer(error))?
            .wait()
            .map(drop)
            .map_err(|(error, _)| RmtError::Transfer(error))
    }

    async fn transmit(&mut self, symbols: &[Self::Symbol]) -> Result<(), Self::Error> {
        // Dropping the transfer stops the channel.
        let mut transfer = self
            .channel
            .reborrow()
            .transmit(symbols)
            .map_err(|(error, _)| RmtError::Transfer(error))?;
        while !transfer.poll() {
            embassy_time::Timer::after_micros(RMT_POLL_MICROS).await;
        }
        transfer
            .wait()
            .map(drop)
            .map_err(|(error, _)| RmtError::Transfer(error))
    }
}

/// One transceiver channel that records the pulse trains on its pin.
pub struct PulseInputChannel {
    channel: esp_hal::rmt::Channel<'static, Blocking, esp_hal::rmt::Rx>,
    config: esp_hal::rmt::RxChannelConfig,
}

/// Configures one receive channel at 1 MHz ticks, ending a reception after
/// 12 ms at one level, and routes it from its pin.
///
/// # Errors
///
/// Returns [`RmtError::Config`] when the channel's memory is taken.
pub fn rmt_input(
    channel: impl esp_hal::rmt::RxChannelCreator<'static, Blocking>,
    pin: impl PeripheralInput<'static>,
) -> Result<PulseInputChannel, RmtError> {
    let config = esp_hal::rmt::RxChannelConfig::default()
        .with_clk_divider(rmt_divider(1_000_000)?)
        .with_idle_threshold(12_000);
    let channel = channel
        .configure_rx(&config)
        .map_err(RmtError::Config)?
        .with_pin(pin);
    Ok(PulseInputChannel { channel, config })
}

impl barracuda_board_hal::PulseInput for PulseInputChannel {
    type Symbol = esp_hal::rmt::PulseCode;
    type Error = RmtError;

    fn empty_symbol() -> Self::Symbol {
        esp_hal::rmt::PulseCode::end_marker()
    }

    fn levels(
        symbol: Self::Symbol,
    ) -> (
        barracuda_board_hal::PulseLevel,
        barracuda_board_hal::PulseLevel,
    ) {
        (
            barracuda_board_hal::PulseLevel::new(symbol.level1() == Level::High, symbol.length1()),
            barracuda_board_hal::PulseLevel::new(symbol.level2() == Level::High, symbol.length2()),
        )
    }

    fn capacity(&self) -> usize {
        if esp_hal::rmt::HAS_RX_WRAP {
            usize::MAX
        } else {
            RMT_RX_BLOCK_SYMBOLS
        }
    }

    fn configure(
        &mut self,
        config: barracuda_board_hal::PulseInputConfig,
    ) -> Result<(), Self::Error> {
        // The glitch filter counts cycles of the 80 MHz clock, at most 255.
        let filter = u64::from(config.filter_ns) * u64::from(RMT_CLOCK_HZ) / 1_000_000_000;
        let updated = self
            .config
            .with_clk_divider(rmt_divider(config.tick_hz)?)
            .with_idle_threshold(config.idle_ticks)
            .with_filter_threshold(u8::try_from(filter).unwrap_or(u8::MAX));
        self.channel
            .apply_config(&updated)
            .map_err(RmtError::Config)?;
        self.config = updated;
        Ok(())
    }

    async fn receive(&mut self, symbols: &mut [Self::Symbol]) -> Result<usize, Self::Error> {
        let limit = symbols.len().min(barracuda_board_hal::PulseInput::capacity(self));
        let symbols = symbols.get_mut(..limit).unwrap_or_default();
        // Dropping the reception stops the channel; the reborrowed channel
        // goes back to `self` either way.
        let mut reception = self
            .channel
            .reborrow()
            .receive(symbols)
            .map_err(|(error, _)| RmtError::Transfer(error))?;
        while !reception.poll() {
            embassy_time::Timer::after_micros(RMT_POLL_MICROS).await;
        }
        reception
            .wait()
            .map(|(count, _)| count)
            .map_err(|(error, _)| RmtError::Transfer(error))
    }
}

/// The Board's LEDC controller, enabled once, which its PWM outputs and the
/// runtime PWM pool share; each takes its own timers and channels.
///
/// The vendor driver keeps the enabled controller in static storage at the
/// Board call site; this handle only reaches its timers and channels, which
/// address the controller's registers by number.
#[cfg(feature = "runtime-pwm")]
#[derive(Clone, Copy)]
pub struct PwmController {
    _enabled: (),
}

#[cfg(feature = "runtime-pwm")]
impl PwmController {
    #[doc(hidden)]
    #[must_use]
    pub const fn __enabled(_controller: &'static Ledc<'static>) -> Self {
        Self { _enabled: () }
    }

    fn timer(self, number: LedcTimerNumber) -> Timer<'static, LowSpeed> {
        Timer::new(esp_hal::peripherals::LEDC::regs(), number)
    }

    fn channel(
        self,
        number: LedcChannelNumber,
        pin: impl PeripheralOutput<'static>,
    ) -> Channel<'static, LowSpeed> {
        Channel::new(number, pin)
    }
}

/// Builds the LEDC controller once, in static storage at the Board call
/// site, with its low-speed timers clocked from APB.
#[macro_export]
#[cfg(feature = "runtime-pwm")]
macro_rules! __barracuda_esp_pwm_controller {
    ($ledc:expr) => {{
        static CONTROLLER: $crate::hal::__StaticCell<
            $crate::hal::__vendor::ledc::Ledc<'static>,
        > = $crate::hal::__StaticCell::new();
        let mut controller = $crate::hal::__vendor::ledc::Ledc::new($ledc);
        controller.set_global_slow_clock($crate::hal::__vendor::ledc::LSGlobalClkSource::APBClk);
        $crate::hal::PwmController::__enabled(CONTROLLER.init(controller))
    }};
}

#[doc(hidden)]
#[cfg(feature = "runtime-pwm")]
pub use __barracuda_esp_pwm_controller as pwm_controller;

/// Duty resolution of a PWM output: ten bits reach about 76 Hz to 78 kHz
/// from an 80 MHz clock, the whole audible range of a buzzer.
#[cfg(feature = "runtime-pwm")]
const PWM_OUTPUT_DUTY: LedcDuty = LedcDuty::Duty10Bit;
#[cfg(feature = "runtime-pwm")]
const PWM_OUTPUT_DUTY_STEPS: u64 = 1 << 10;
/// Largest LEDC clock divisor, eight fractional bits included.
#[cfg(feature = "runtime-pwm")]
const PWM_OUTPUT_MAX_DIVISOR: u64 = 0x3_ffff;
#[cfg(feature = "runtime-pwm")]
const PWM_OUTPUT_INITIAL_HZ: u32 = 1_000;

/// One LEDC timer and channel driving one pin at a retunable frequency.
///
/// The channel keeps the timer it was configured with in static storage;
/// retuning reprograms the same hardware timer through a second handle with
/// the same duty resolution, so the channel's duty stays a share of the
/// period.
#[cfg(feature = "runtime-pwm")]
pub struct PwmOutput {
    channel: Channel<'static, LowSpeed>,
    retune: Timer<'static, LowSpeed>,
    source_hz: u32,
}

/// Configures one LEDC timer at 1 kHz and its channel, silent, on `pin`.
///
/// # Errors
///
/// Returns [`ExposedPwmError`] when the timer or channel rejects its
/// configuration.
#[cfg(feature = "runtime-pwm")]
pub fn pwm_output(
    controller: PwmController,
    timer: RuntimePwmTimer,
    channel: LedcChannelNumber,
    pin: impl PeripheralOutput<'static>,
) -> Result<PwmOutput, ExposedPwmError> {
    let config = LedcTimerConfig {
        duty: PWM_OUTPUT_DUTY,
        clock_source: LSClockSource::APBClk,
        frequency: Rate::from_hz(PWM_OUTPUT_INITIAL_HZ),
    };
    let mut configured = controller.timer(timer.number);
    configured
        .configure(config)
        .map_err(ExposedPwmError::Timer)?;
    let source_hz = configured.freq().map_or(0, |rate| rate.as_hz());
    let configured: &'static Timer<'static, LowSpeed> = timer.storage.init(configured);
    let mut output = controller.channel(channel, pin);
    output
        .configure(esp_hal::ledc::channel::config::Config {
            timer: configured,
            duty_pct: 0,
            drive_mode: DriveMode::PushPull,
        })
        .map_err(ExposedPwmError::Channel)?;
    Ok(PwmOutput {
        channel: output,
        retune: controller.timer(timer.number),
        source_hz,
    })
}

// SAFETY: a `PwmOutput` is the only owner of its LEDC timer and channel. The
// references inside the vendor handles point at the LEDC register block and
// at the timer descriptor in static storage; neither is tied to the task or
// core that built them, and only this owner touches its timer's and
// channel's registers.
#[allow(unsafe_code)]
#[cfg(feature = "runtime-pwm")]
unsafe impl Send for PwmOutput {}

#[cfg(feature = "runtime-pwm")]
impl embedded_hal::pwm::ErrorType for PwmOutput {
    type Error = LedcChannelError;
}

#[cfg(feature = "runtime-pwm")]
impl embedded_hal::pwm::SetDutyCycle for PwmOutput {
    fn max_duty_cycle(&self) -> u16 {
        self.channel.max_duty_cycle()
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        self.channel.set_duty_cycle(duty)
    }
}

#[cfg(feature = "runtime-pwm")]
impl barracuda_board_hal::PwmFrequencyControl for PwmOutput {
    fn frequency_range(&self) -> (u32, u32) {
        // The timer divides its clock by 1 to 1024 (with eight fractional
        // bits) and counts `PWM_OUTPUT_DUTY_STEPS` per period.
        let source = u64::from(self.source_hz);
        let lowest = (source << 8).div_ceil(PWM_OUTPUT_MAX_DIVISOR * PWM_OUTPUT_DUTY_STEPS);
        let highest = source / PWM_OUTPUT_DUTY_STEPS;
        (
            u32::try_from(lowest).unwrap_or(u32::MAX),
            u32::try_from(highest).unwrap_or(u32::MAX),
        )
    }

    fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error> {
        if frequency_hz == 0 {
            return Err(LedcChannelError::Timer);
        }
        self.retune
            .configure(LedcTimerConfig {
                duty: PWM_OUTPUT_DUTY,
                clock_source: LSClockSource::APBClk,
                frequency: Rate::from_hz(frequency_hz),
            })
            .map_err(|_| LedcChannelError::Timer)
    }
}
