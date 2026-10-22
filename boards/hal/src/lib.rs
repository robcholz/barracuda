//! Board HAL composition contract.
//!
//! A Board HAL owns peripheral implementation construction and returns semantic
//! peripherals. It does not construct or contain Platform resources.

#![no_std]

extern crate alloc;

mod providers;
mod runtime;

use core::{cell::RefCell, convert::Infallible, future::Future};

use embassy_executor::Spawner;
use embedded_hal::{
    digital::{InputPin, OutputPin, StatefulOutputPin},
    i2c, spi,
};

pub use providers::{
    AnalogProvider, DigitalProvider, I2cProvider, I2cRequest, I2sProvider, I2sRequest, PwmProvider,
    PwmRequest, RuntimeAnalogPlatform, RuntimeI2sPlatform, RuntimeIo, RuntimeOpenError,
    RuntimePlatform, RuntimePwmPlatform, RuntimeUartPlatform, SpiProvider, SpiRequest, UartConfig,
    UartDataBits, UartParity, UartProvider, UartRequest, UartStopBits, UnsupportedFunction,
};
pub use runtime::{LeaseError, ResourceKind};

/// Stable attached audio peripheral API.
pub use barracuda_peripheral::audio;
/// Stable attached-button peripheral API.
pub use barracuda_peripheral::buttons;
/// Stable attached buzzer peripheral API.
pub use barracuda_peripheral::buzzer;
/// Stable attached camera peripheral API.
pub use barracuda_peripheral::camera;
/// Stable attached display peripheral API.
pub use barracuda_peripheral::display;
/// Stable attached EEPROM peripheral API.
pub use barracuda_peripheral::eeprom;
/// Stable attached environment-sensor peripheral API.
pub use barracuda_peripheral::environment;
/// Stable attached inertial-measurement peripheral API.
pub use barracuda_peripheral::imu;
/// Stable attached indicator peripheral API.
pub use barracuda_peripheral::indicator;
/// Stable attached infrared remote-control API.
pub use barracuda_peripheral::infrared;
/// Stable attached keyboard peripheral API.
pub use barracuda_peripheral::keyboard;
/// Stable attached LED-strip peripheral API.
pub use barracuda_peripheral::led_strip;
/// Stable attached electrical-power monitoring API.
pub use barracuda_peripheral::power;
/// Stable real-time clock API.
pub use barracuda_peripheral::real_time_clock;
/// Stable attached removable-filesystem peripheral API.
pub use barracuda_peripheral::removable_storage;
/// Stable attached rotary-encoder peripheral API.
pub use barracuda_peripheral::rotary_encoder;
/// Stable attached secure-element peripheral API.
pub use barracuda_peripheral::secure_element;
/// Stable touch-input API.
pub use barracuda_peripheral::touch;
/// Stable attached vibration-motor API.
pub use barracuda_peripheral::vibration;
/// Stable factory contract implemented by every peripheral implementation.
pub use barracuda_peripheral::{display_set, Instance, PeripheralImplementation};

/// Statically allocated owner for one blocking I2C bus shared by attached peripherals.
pub struct SharedI2cBus<Bus> {
    bus: critical_section::Mutex<RefCell<Bus>>,
}

impl<Bus> SharedI2cBus<Bus> {
    /// Wraps one exclusively owned bus for address-level sharing.
    #[must_use]
    pub const fn new(bus: Bus) -> Self {
        Self {
            bus: critical_section::Mutex::new(RefCell::new(bus)),
        }
    }

    /// Creates another standard blocking I2C view over this owner.
    #[must_use]
    pub fn device(&'static self) -> embedded_hal_bus::i2c::CriticalSectionDevice<'static, Bus> {
        embedded_hal_bus::i2c::CriticalSectionDevice::new(&self.bus)
    }
}

/// Input bias selected while a exposed GPIO operates as an input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pull {
    /// Leave the input electrically unbiased.
    #[default]
    None,
    /// Enable the pin's pull-up resistor.
    Up,
    /// Enable the pin's pull-down resistor.
    Down,
}

/// Electrical output driver selected for a exposed GPIO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputDrive {
    /// Actively drive both high and low levels.
    #[default]
    PushPull,
    /// Actively drive low and release the line for a high level.
    OpenDrain,
}

/// Digital latch level used while changing a exposed GPIO to output mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DigitalLevel {
    /// Drive the logical low level.
    #[default]
    Low,
    /// Drive the logical high level.
    High,
}

/// Runtime configuration for a exposed digital input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputConfig {
    /// Input bias.
    pub pull: Pull,
}

/// Runtime configuration for a exposed digital output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputConfig {
    /// Output latch installed before enabling the driver.
    pub initial: DigitalLevel,
    /// Electrical output driver.
    pub drive: OutputDrive,
}

/// An exposed GPIO whose runtime owner may change its digital mode.
///
/// Reads and writes remain standard `embedded-hal` operations. This extension
/// exists for a runtime owner that switches a pin's mode, because
/// `embedded-hal` does not standardize runtime transitions between disabled,
/// input, and output modes.
pub trait ConfigurableDigitalPin: InputPin + StatefulOutputPin {
    /// Configures the pin as a digital input.
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error>;

    /// Installs the initial latch and configures the pin as an output.
    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error>;

    /// Places the pin in its Platform-defined disconnected state.
    fn disable(&mut self) -> Result<(), Self::Error>;
}

/// One selected half-duplex SPI device with four bidirectional data lines.
///
/// The opcode and 24-bit address phases are transmitted on one data line;
/// payload bytes are transmitted on all four lines. This narrow contract keeps
/// controller-specific QSPI details in the Platform while display protocols
/// remain ordinary chip and peripheral implementations.
pub trait QuadSpiBus {
    /// Transport failure returned by the Platform adapter.
    type Error: core::fmt::Debug;

    /// Writes one opcode, one 24-bit address, and an optional payload.
    fn write(&mut self, opcode: u8, address: u32, data: &[u8]) -> Result<(), Self::Error>;

    /// Writes one opcode, one 24-bit address, and an optional payload, the
    /// payload also on one data line. Display controllers take register
    /// parameters this way and only pixel data on four lines.
    fn write_single_line(
        &mut self,
        opcode: u8,
        address: u32,
        data: &[u8],
    ) -> Result<(), Self::Error>;
}

/// Two-channel capacitive-touch sampling transport supplied by a Platform.
pub trait CapacitiveTouchChannels {
    /// Sampling failure returned by the Platform adapter.
    type Error: core::fmt::Debug;

    /// Reads the two hardware capacitance counters together.
    fn read(&mut self) -> Result<[u32; 2], Self::Error>;
}

/// Quadrature edge count supplied by a Platform's pulse-counter unit.
///
/// `embedded-hal` has no encoder contract. The Platform counts every edge of
/// both encoder signals, up while B leads A and down while A leads B, and
/// widens the hardware counter in software: a reader that looks at least
/// once every 16 384 edges sees every one.
pub trait QuadratureDecoder {
    /// Counter failure returned by the Platform adapter.
    type Error: core::fmt::Debug;

    /// Returns the signed number of edges counted since construction.
    fn count(&mut self) -> Result<i64, Self::Error>;
}

/// Error family shared by one exposed analog peripheral.
pub trait AnalogErrorType {
    /// Error returned by analog conversions.
    type Error: core::error::Error;
}

/// One Board-exposed analog input channel.
pub trait AnalogInput: AnalogErrorType {
    /// Highest raw sample value produced by this channel.
    fn max_value(&self) -> u32;

    /// Samples the channel in its native integer range.
    fn read(&mut self) -> Result<u32, Self::Error>;
}

/// One analog input a peripheral implementation samples in millivolts.
///
/// The Platform applies its converter's calibration, so implementations such
/// as a battery monitor behind a resistor divider or a resistor-ladder button
/// array compare voltages instead of raw converter codes, which differ per
/// chip and per attenuation. `embedded-hal` 1.0 has no analog contract.
pub trait AnalogVoltageInput: AnalogErrorType {
    /// Samples the pin once and returns its voltage in millivolts.
    fn read_millivolts(&mut self) -> Result<u32, Self::Error>;
}

/// Bus control an SD card in SPI mode needs beyond `embedded-hal`'s
/// `SpiDevice`.
///
/// The SD specification identifies a card at no more than 400 kHz, then runs
/// it at its full clock, and wakes it with at least 74 clock cycles while its
/// chip select is high. An `SpiDevice` fixes its clock and always asserts its
/// chip select, so a Platform's SPI device type also implements this.
pub trait SpiDeviceControl: embedded_hal::spi::ErrorType {
    /// Returns the clock of this device's following transactions, initially
    /// the one the Board declares.
    fn frequency_hz(&self) -> u32;

    /// Sets the clock of this device's following transactions.
    fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error>;

    /// Clocks `bytes` out with this device's chip select released.
    fn write_deselected(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

/// Bus control a bit-oriented protocol on an SPI controller needs beyond
/// `embedded-hal`'s `SpiBus`: the clock and mode, and transfers shorter than
/// a byte.
///
/// A JTAG test-access port driven through an SPI controller (TCK on SCK, TDI
/// on MOSI, TDO on MISO, TMS on a GPIO the implementation owns) moves between
/// states one clock at a time, which a byte-oriented `SpiBus` cannot clock.
/// A Platform's `spi-bus` type implements this where its controller can.
pub trait SpiBusControl: embedded_hal::spi::ErrorType {
    /// Applies `frequency_hz` and `mode` to the following transfers.
    fn configure(
        &mut self,
        frequency_hz: u32,
        mode: embedded_hal::spi::Mode,
    ) -> Result<(), Self::Error>;

    /// Clocks out the low `bits` bits of `value`, 1 through 8, most
    /// significant first, and returns once the last has been clocked.
    fn write_bits(&mut self, value: u8, bits: u8) -> Result<(), Self::Error>;
}

/// One half of a pulse symbol: an output level held for a number of ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PulseLevel {
    /// Whether the line is high.
    pub high: bool,
    /// How long the level lasts, in ticks of the channel's resolution.
    pub ticks: u16,
}

impl PulseLevel {
    /// Longest level one symbol half holds.
    pub const MAX_TICKS: u16 = 0x7fff;

    /// Creates one level; `ticks` above [`Self::MAX_TICKS`] mean the maximum.
    #[must_use]
    pub const fn new(high: bool, ticks: u16) -> Self {
        Self {
            high,
            ticks: if ticks > Self::MAX_TICKS {
                Self::MAX_TICKS
            } else {
                ticks
            },
        }
    }
}

/// Carrier a pulse output modulates onto its active level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PulseCarrier {
    /// Carrier frequency in hertz.
    pub frequency_hz: u32,
    /// Share of each carrier period the line is active, 1 through 99 percent.
    pub duty_percent: u8,
}

/// Timing and line configuration of one pulse output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PulseOutputConfig {
    /// Tick frequency symbol lengths count in.
    pub tick_hz: u32,
    /// Level the line rests at between transmissions; its opposite is the
    /// active level.
    pub idle_high: bool,
    /// Carrier modulated onto the active level, if any.
    pub carrier: Option<PulseCarrier>,
}

/// Timing configuration of one pulse input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PulseInputConfig {
    /// Tick frequency received symbol lengths count in.
    pub tick_hz: u32,
    /// A level lasting this many ticks ends a reception.
    pub idle_ticks: u16,
    /// Pulses shorter than this many nanoseconds are ignored as glitches.
    pub filter_ns: u32,
}

/// A hardware pulse-train generator: one output channel that clocks out
/// level and duration pairs on its own, as WS2812 LED chains and infrared
/// transmitters need. `embedded-hal` has no such contract.
///
/// Symbols are the Platform's own representation, so a sequence built once
/// is handed to the hardware without conversion. A sequence ends with
/// [`PulseOutput::end_marker`].
pub trait PulseOutput {
    /// One hardware symbol: two levels in order.
    type Symbol: Copy;
    /// Configuration or transfer failure.
    type Error: core::fmt::Debug;

    /// Builds one symbol. A zero-tick second level ends the transmission
    /// after the first.
    fn symbol(first: PulseLevel, second: PulseLevel) -> Self::Symbol;

    /// Returns the symbol that ends a sequence.
    fn end_marker() -> Self::Symbol;

    /// Applies the tick resolution, idle level, and carrier for the following
    /// transmissions.
    fn configure(&mut self, config: PulseOutputConfig) -> Result<(), Self::Error>;

    /// Transmits `symbols` and returns when the last has been sent, keeping
    /// the CPU busy so the hardware is refilled without delay.
    fn transmit_blocking(&mut self, symbols: &[Self::Symbol]) -> Result<(), Self::Error>;

    /// Transmits `symbols`, waiting for the end without blocking the
    /// executor.
    fn transmit(
        &mut self,
        symbols: &[Self::Symbol],
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// A hardware pulse-train recorder: one input channel that measures the
/// level and duration pairs of a signal, such as an infrared receiver's.
pub trait PulseInput {
    /// One hardware symbol: two levels in order.
    type Symbol: Copy;
    /// Configuration or transfer failure.
    type Error: core::fmt::Debug;

    /// Returns a cleared symbol to fill a receive buffer with.
    fn empty_symbol() -> Self::Symbol;

    /// Splits one received symbol into its two levels; a zero-tick level
    /// ends the reception.
    fn levels(symbol: Self::Symbol) -> (PulseLevel, PulseLevel);

    /// Returns the most symbols one reception records; a receive buffer
    /// longer than this is used only up to it.
    fn capacity(&self) -> usize;

    /// Applies the tick resolution, idle threshold, and glitch filter for the
    /// following receptions.
    fn configure(&mut self, config: PulseInputConfig) -> Result<(), Self::Error>;

    /// Waits for one signal and records it into `symbols`, returning how many
    /// symbols it filled. Dropping the future stops the reception.
    fn receive(
        &mut self,
        symbols: &mut [Self::Symbol],
    ) -> impl Future<Output = Result<usize, Self::Error>>;
}

/// Frequency control a tone output needs beyond `embedded-hal`'s
/// `SetDutyCycle`, whose frequency is fixed: a buzzer changes pitch by
/// retuning its PWM timer. The duty cycle keeps its share of the period.
pub trait PwmFrequencyControl: embedded_hal::pwm::SetDutyCycle {
    /// Returns the lowest and highest frequency the output reaches, in hertz.
    fn frequency_range(&self) -> (u32, u32);

    /// Retunes the output to `frequency_hz`.
    fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error>;
}

/// One Board-exposed analog output channel.
pub trait AnalogOutput: AnalogErrorType {
    /// Highest raw value accepted by this channel.
    fn max_value(&self) -> u32;

    /// Writes one value in the channel's native integer range.
    fn write(&mut self, value: u32) -> Result<(), Self::Error>;
}

/// Unified owner of physical resources explicitly exposed by one Board.
///
/// Protocol-specific provider traits are implemented on this same value. They
/// atomically claim its move-only tokens before constructing standard HAL
/// values, preventing protocol packages from creating conflicting views.
pub trait ExposedIo: Send + Sync + 'static {}

/// Resources produced by one selected Board HAL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoardResources<Peripherals, ExposedIo> {
    /// Optional initialized peripherals physically attached to the Board.
    pub peripherals: Peripherals,
    /// Explicitly exposed Board I/O resources.
    pub exposed_io: ExposedIo,
}

impl<Peripherals, ExposedIo> BoardResources<Peripherals, ExposedIo> {
    /// Combines the two independently named Board hardware surfaces.
    #[must_use]
    pub const fn new(peripherals: Peripherals, exposed_io: ExposedIo) -> Self {
        Self {
            peripherals,
            exposed_io,
        }
    }
}

/// Result of initializing one statically selected [`BoardHal`].
pub type BoardHalInitResult<H> = Result<<H as BoardHal>::Resources, <H as BoardHal>::Error>;

/// Statically composed Board matrix and peripheral implementations.
pub trait BoardHal: Sized + 'static {
    /// Move-only chip resources assigned to this Board HAL by Target.
    type Bindings;
    /// Semantic hardware peripherals exposed to System or Plugins.
    type Resources;
    /// Board HAL initialization failure.
    type Error;

    /// Initializes peripherals for one concrete Board matrix.
    fn initialize(
        spawner: Spawner,
        bindings: Self::Bindings,
    ) -> impl Future<Output = BoardHalInitResult<Self>>;
}

/// HAL for desktop Boards that declare no peripherals.
pub struct EmptyBoardHal;

/// Explicit absence of attached peripherals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoPeripherals;

/// Uninhabited LED-strip peripheral used when a Board has no attached strip.
pub struct UnavailableLedStrip {
    never: Infallible,
}

impl led_strip::LedStrip for UnavailableLedStrip {
    type Error = Infallible;

    fn len(&self) -> usize {
        match self.never {}
    }

    fn write(&mut self, _pixels: &[led_strip::Rgb8]) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl led_strip::LedStripPeripheral for NoPeripherals {
    type LedStrip = UnavailableLedStrip;

    fn take_led_strip(&mut self, _instance: Instance) -> Option<Self::LedStrip> {
        None
    }
}

/// Uninhabited camera peripheral used when a Board has no attached camera.
pub struct UnavailableCamera {
    never: Infallible,
}

impl camera::Camera for UnavailableCamera {
    type Error = Infallible;

    fn descriptor(&self) -> camera::CameraDescriptor {
        match self.never {}
    }

    async fn capture<'a>(
        &'a mut self,
        _buffer: &'a mut [u8],
    ) -> Result<camera::CapturedFrame, Self::Error> {
        match self.never {}
    }
}

impl camera::CameraPeripheral for NoPeripherals {
    type Camera = UnavailableCamera;

    fn take_camera(&mut self, _instance: Instance) -> Option<Self::Camera> {
        None
    }
}

/// Uninhabited IMU peripheral used when a Board has no attached sensor.
pub struct UnavailableImu {
    never: Infallible,
}

impl imu::Imu for UnavailableImu {
    type Error = Infallible;

    fn descriptor(&self) -> imu::ImuDescriptor {
        match self.never {}
    }

    async fn read_sample(&mut self) -> Result<imu::ImuSample, Self::Error> {
        match self.never {}
    }
}

impl imu::ImuPeripheral for NoPeripherals {
    type Imu = UnavailableImu;

    fn take_imu(&mut self, _instance: Instance) -> Option<Self::Imu> {
        None
    }
}

/// Uninhabited audio-codec peripheral used when a Board has no attached codec.
pub struct UnavailableAudioCodec {
    never: Infallible,
}

impl audio::AudioCodec for UnavailableAudioCodec {
    type Error = Infallible;

    fn descriptor(&self) -> audio::AudioDescriptor {
        match self.never {}
    }

    fn set_output_volume(&mut self, _volume: u8) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn write(&mut self, _samples: &[i16]) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn read(&mut self, _samples: &mut [i16]) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl audio::AudioCodecPeripheral for NoPeripherals {
    type AudioCodec = UnavailableAudioCodec;

    fn take_audio_codec(&mut self, _instance: Instance) -> Option<Self::AudioCodec> {
        None
    }
}

/// Uninhabited display peripheral used when a Board has no attached display.
pub struct UnavailableDisplay {
    never: Infallible,
}

impl embedded_graphics_core::geometry::OriginDimensions for UnavailableDisplay {
    fn size(&self) -> embedded_graphics_core::geometry::Size {
        match self.never {}
    }
}

impl embedded_graphics_core::draw_target::DrawTarget for UnavailableDisplay {
    type Color = embedded_graphics_core::pixelcolor::Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, _pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics_core::Pixel<Self::Color>>,
    {
        match self.never {}
    }
}

impl display::Display for UnavailableDisplay {
    type ControlError = Infallible;
    type RenderError = Infallible;

    fn descriptor(&self) -> display::DisplayDescriptor {
        match self.never {}
    }

    fn draw_rgb565(
        &mut self,
        _area: embedded_graphics_core::primitives::Rectangle,
        _pixels: &[embedded_graphics_core::pixelcolor::Rgb565],
    ) -> Result<(), Self::RenderError> {
        match self.never {}
    }

    async fn flush(&mut self, _request: display::RefreshRequest) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_power(&mut self, _power: display::DisplayPower) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_orientation(
        &mut self,
        _orientation: display::DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        match self.never {}
    }
}

impl display::DisplayPeripheral for NoPeripherals {
    type Display = UnavailableDisplay;

    fn take_display(&mut self, _instance: Instance) -> Option<Self::Display> {
        None
    }
}

/// Uninhabited slot used when a Board has no removable filesystem.
pub struct UnavailableRemovableStorage {
    never: Infallible,
}

impl removable_storage::RemovableStorage for UnavailableRemovableStorage {
    fn slot_id(&self) -> &'static str {
        match self.never {}
    }

    fn status(&self) -> removable_storage::RemovableStorageStatus {
        match self.never {}
    }

    async fn next_event(&mut self) -> removable_storage::RemovableStorageEvent {
        match self.never {}
    }
}

impl removable_storage::RemovableStoragePeripheral for NoPeripherals {
    type RemovableStorage = UnavailableRemovableStorage;

    fn take_removable_storage(&mut self) -> Option<Self::RemovableStorage> {
        None
    }
}

/// Uninhabited power-monitor peripheral used when a Board has no attached monitor.
pub struct UnavailablePowerMonitor {
    never: Infallible,
}

impl power::PowerMonitor for UnavailablePowerMonitor {
    type Error = Infallible;

    fn measure(&mut self) -> Result<power::PowerMeasurement, Self::Error> {
        match self.never {}
    }
}

impl power::PowerMonitorPeripheral for NoPeripherals {
    type PowerMonitor = UnavailablePowerMonitor;

    fn take_power_monitor(&mut self, _instance: Instance) -> Option<Self::PowerMonitor> {
        None
    }
}

/// Uninhabited real-time clock used when a Board has no attached clock.
pub struct UnavailableRealTimeClock {
    never: Infallible,
}

impl real_time_clock::RealTimeClock for UnavailableRealTimeClock {
    type Error = Infallible;

    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        match self.never {}
    }

    fn read_datetime(&mut self) -> Result<real_time_clock::DateTime, Self::Error> {
        match self.never {}
    }

    fn set_datetime(&mut self, _datetime: real_time_clock::DateTime) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl real_time_clock::RealTimeClockPeripheral for NoPeripherals {
    type RealTimeClock = UnavailableRealTimeClock;

    fn take_real_time_clock(&mut self, _instance: Instance) -> Option<Self::RealTimeClock> {
        None
    }
}

/// Uninhabited touch input used when a Board has no attached touch controller.
pub struct UnavailableTouch {
    never: Infallible,
}

impl touch::Touch for UnavailableTouch {
    type Error = Infallible;

    fn descriptor(&self) -> touch::TouchDescriptor {
        match self.never {}
    }

    fn read_frame(&mut self) -> Result<touch::TouchFrame, Self::Error> {
        match self.never {}
    }
}

impl touch::TouchPeripheral for NoPeripherals {
    type Touch = UnavailableTouch;

    fn take_touch(&mut self, _instance: Instance) -> Option<Self::Touch> {
        None
    }
}

/// Uninhabited attached-button input used when a Board declares no buttons.
pub struct UnavailableButtons {
    never: Infallible,
}

impl buttons::Buttons for UnavailableButtons {
    type Error = Infallible;

    fn read(&mut self) -> Result<buttons::ButtonSnapshot, Self::Error> {
        match self.never {}
    }
}

impl buttons::ButtonsPeripheral for NoPeripherals {
    type Buttons = UnavailableButtons;

    fn take_buttons(&mut self, _instance: Instance) -> Option<Self::Buttons> {
        None
    }
}

/// Uninhabited environment sensor used when a Board declares none.
pub struct UnavailableEnvironmentSensor {
    never: Infallible,
}

impl environment::EnvironmentSensor for UnavailableEnvironmentSensor {
    type Error = Infallible;

    fn quantities(&self) -> environment::Quantities {
        match self.never {}
    }

    async fn read(&mut self) -> Result<environment::EnvironmentReading, Self::Error> {
        match self.never {}
    }
}

impl environment::EnvironmentSensorPeripheral for NoPeripherals {
    type EnvironmentSensor = UnavailableEnvironmentSensor;

    fn take_environment_sensor(&mut self, _instance: Instance) -> Option<Self::EnvironmentSensor> {
        None
    }
}

/// Uninhabited keyboard used when a Board declares no keyboard.
pub struct UnavailableKeyboard {
    never: Infallible,
}

impl keyboard::Keyboard for UnavailableKeyboard {
    type Error = Infallible;

    fn poll_event(&mut self) -> Result<Option<keyboard::KeyEvent>, Self::Error> {
        match self.never {}
    }
}

impl keyboard::KeyboardPeripheral for NoPeripherals {
    type Keyboard = UnavailableKeyboard;

    fn take_keyboard(&mut self, _instance: Instance) -> Option<Self::Keyboard> {
        None
    }
}

/// Uninhabited rotary encoder used when a Board declares no encoder.
pub struct UnavailableRotaryEncoder {
    never: Infallible,
}

impl rotary_encoder::RotaryEncoder for UnavailableRotaryEncoder {
    type Error = Infallible;

    fn position(&mut self) -> Result<i64, Self::Error> {
        match self.never {}
    }

    fn delta(&mut self) -> Result<i64, Self::Error> {
        match self.never {}
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl rotary_encoder::RotaryEncoderPeripheral for NoPeripherals {
    type RotaryEncoder = UnavailableRotaryEncoder;

    fn take_rotary_encoder(&mut self, _instance: Instance) -> Option<Self::RotaryEncoder> {
        None
    }
}

/// Uninhabited buzzer used when a Board has no attached buzzer.
pub struct UnavailableBuzzer {
    never: Infallible,
}

impl buzzer::Buzzer for UnavailableBuzzer {
    type Error = Infallible;

    fn frequency_range(&self) -> (u32, u32) {
        match self.never {}
    }

    fn start(&mut self, _frequency_hz: u32, _volume: buzzer::Volume) -> Result<(), Self::Error> {
        match self.never {}
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn play_note(
        &mut self,
        _note: buzzer::Note,
        _volume: buzzer::Volume,
    ) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl buzzer::BuzzerPeripheral for NoPeripherals {
    type Buzzer = UnavailableBuzzer;

    fn take_buzzer(&mut self, _instance: Instance) -> Option<Self::Buzzer> {
        None
    }
}

/// Uninhabited vibration motor used when a Board has no attached motor.
pub struct UnavailableVibration {
    never: Infallible,
}

impl vibration::Vibration for UnavailableVibration {
    type Error = Infallible;

    fn has_variable_strength(&self) -> bool {
        match self.never {}
    }

    fn set(&mut self, _strength: vibration::Strength) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn pulse(
        &mut self,
        _pattern: &[vibration::Pulse],
        _strength: vibration::Strength,
    ) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl vibration::VibrationPeripheral for NoPeripherals {
    type Vibration = UnavailableVibration;

    fn take_vibration(&mut self, _instance: Instance) -> Option<Self::Vibration> {
        None
    }
}

/// Uninhabited infrared transceiver used when a Board has none.
pub struct UnavailableInfrared {
    never: Infallible,
}

impl infrared::Infrared for UnavailableInfrared {
    type Error = Infallible;

    fn can_receive(&self) -> bool {
        match self.never {}
    }

    async fn send_nec(&mut self, _command: infrared::NecCommand) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn send_raw(
        &mut self,
        _carrier: infrared::Carrier,
        _timings_us: &[u16],
    ) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn receive(
        &mut self,
        _timings_us: &mut [u16],
    ) -> Result<infrared::Received, Self::Error> {
        match self.never {}
    }
}

impl infrared::InfraredPeripheral for NoPeripherals {
    type Infrared = UnavailableInfrared;

    fn take_infrared(&mut self, _instance: Instance) -> Option<Self::Infrared> {
        None
    }
}

/// Uninhabited EEPROM used when a Board has none.
pub struct UnavailableEeprom {
    never: Infallible,
}

impl eeprom::Eeprom for UnavailableEeprom {
    type Error = Infallible;

    fn geometry(&self) -> eeprom::EepromGeometry {
        match self.never {}
    }

    async fn read(&mut self, _offset: u32, _buffer: &mut [u8]) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn write(&mut self, _offset: u32, _data: &[u8]) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl eeprom::EepromPeripheral for NoPeripherals {
    type Eeprom = UnavailableEeprom;

    fn take_eeprom(&mut self, _instance: Instance) -> Option<Self::Eeprom> {
        None
    }
}

/// Uninhabited secure element used when a Board has none.
pub struct UnavailableSecureElement {
    never: Infallible,
}

impl secure_element::SecureElement for UnavailableSecureElement {
    type Error = Infallible;

    async fn serial_number(&mut self) -> Result<secure_element::SerialNumber, Self::Error> {
        match self.never {}
    }

    async fn fill_random(&mut self, _buffer: &mut [u8]) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn public_key(
        &mut self,
        _slot: secure_element::KeySlot,
    ) -> Result<secure_element::P256PublicKey, Self::Error> {
        match self.never {}
    }

    async fn sign_digest(
        &mut self,
        _slot: secure_element::KeySlot,
        _digest: &[u8; 32],
    ) -> Result<secure_element::P256Signature, Self::Error> {
        match self.never {}
    }
}

impl secure_element::SecureElementPeripheral for NoPeripherals {
    type SecureElement = UnavailableSecureElement;

    fn take_secure_element(&mut self, _instance: Instance) -> Option<Self::SecureElement> {
        None
    }
}

/// Explicit absence of exposed Board I/O peripherals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoExposedIo;

/// Uninhabited GPIO value that makes an empty I/O surface fully typed.
pub struct UnavailableGpio {
    never: Infallible,
}

/// Uninhabited analog input used by Boards without runtime analog support.
pub struct UnavailableAnalogInput {
    never: Infallible,
}

impl AnalogErrorType for UnavailableAnalogInput {
    type Error = Infallible;
}

impl AnalogInput for UnavailableAnalogInput {
    fn max_value(&self) -> u32 {
        match self.never {}
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        match self.never {}
    }
}

/// Uninhabited analog output used by Boards without runtime analog support.
pub struct UnavailableAnalogOutput {
    never: Infallible,
}

/// Uninhabited PWM output used by Boards without runtime PWM support.
pub struct UnavailablePwm {
    never: Infallible,
}

/// Uninhabited UART stream used by Boards without runtime UART support.
pub struct UnavailableUart {
    never: Infallible,
}

impl embedded_io::ErrorType for UnavailableUart {
    type Error = Infallible;
}

impl embedded_io_async::Read for UnavailableUart {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
        match self.never {}
    }
}

impl embedded_io_async::Write for UnavailableUart {
    async fn write(&mut self, _buffer: &[u8]) -> Result<usize, Self::Error> {
        match self.never {}
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match self.never {}
    }
}

/// Uninhabited PCM stream used by Boards without runtime I2S support.
pub struct UnavailableI2s {
    never: Infallible,
}

impl audio::PcmStream for UnavailableI2s {
    type Error = Infallible;

    fn format(&self) -> audio::PcmFormat {
        match self.never {}
    }

    async fn write(&mut self, _samples: &[i16]) -> Result<(), Self::Error> {
        match self.never {}
    }

    async fn read(&mut self, _samples: &mut [i16]) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl embedded_hal::pwm::ErrorType for UnavailablePwm {
    type Error = Infallible;
}

impl embedded_hal::pwm::SetDutyCycle for UnavailablePwm {
    fn max_duty_cycle(&self) -> u16 {
        match self.never {}
    }

    fn set_duty_cycle(&mut self, _duty: u16) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl AnalogErrorType for UnavailableAnalogOutput {
    type Error = Infallible;
}

impl AnalogOutput for UnavailableAnalogOutput {
    fn max_value(&self) -> u32 {
        match self.never {}
    }

    fn write(&mut self, _value: u32) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl UnavailableGpio {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl embedded_hal::digital::ErrorType for UnavailableGpio {
    type Error = Infallible;
}

impl InputPin for UnavailableGpio {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }
}

impl OutputPin for UnavailableGpio {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

impl StatefulOutputPin for UnavailableGpio {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }
}

impl ConfigurableDigitalPin for UnavailableGpio {
    fn configure_input(&mut self, _config: InputConfig) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn configure_output(&mut self, _config: OutputConfig) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

/// Uninhabited I2C value that makes an empty I/O surface fully typed.
pub struct UnavailableI2c {
    never: Infallible,
}

impl UnavailableI2c {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl i2c::ErrorType for UnavailableI2c {
    type Error = Infallible;
}

impl embedded_hal_async::i2c::I2c for UnavailableI2c {
    async fn transaction(
        &mut self,
        _address: u8,
        _operations: &mut [i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

/// Uninhabited SPI value that makes an empty I/O surface fully typed.
pub struct UnavailableSpi {
    never: Infallible,
}

impl UnavailableSpi {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl spi::ErrorType for UnavailableSpi {
    type Error = Infallible;
}

impl embedded_hal_async::spi::SpiBus for UnavailableSpi {
    async fn read(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn write(&mut self, _words: &[u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn transfer(&mut self, _read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

impl ExposedIo for NoExposedIo {}

impl DigitalProvider for NoExposedIo {
    type Pin = UnavailableGpio;
    type Error = UnsupportedFunction;

    fn digital_available(&self, _name: &str) -> bool {
        false
    }

    fn acquire_digital(&self, _name: &str) -> Result<Self::Pin, Self::Error> {
        Err(UnsupportedFunction::new("digital I/O"))
    }
}

impl AnalogProvider for NoExposedIo {
    type Input = UnavailableAnalogInput;
    type Output = UnavailableAnalogOutput;
    type Error = UnsupportedFunction;

    fn analog_input_available(&self, _name: &str) -> bool {
        false
    }

    fn analog_output_available(&self, _name: &str) -> bool {
        false
    }

    fn acquire_analog_input(&self, _name: &str) -> Result<Self::Input, Self::Error> {
        Err(UnsupportedFunction::new("analog input"))
    }

    fn acquire_analog_output(&self, _name: &str) -> Result<Self::Output, Self::Error> {
        Err(UnsupportedFunction::new("analog output"))
    }
}

impl PwmProvider for NoExposedIo {
    type Output = UnavailablePwm;
    type Error = UnsupportedFunction;

    fn pwm_available(&self, _name: &str) -> bool {
        false
    }

    fn open_pwm(&self, _request: PwmRequest<'_>) -> Result<Self::Output, Self::Error> {
        Err(UnsupportedFunction::new("PWM"))
    }
}

impl UartProvider for NoExposedIo {
    type Port = UnavailableUart;
    type Error = UnsupportedFunction;

    fn uart_available(&self, _tx: Option<&str>, _rx: Option<&str>) -> bool {
        false
    }

    fn open_uart(&self, _request: UartRequest<'_>) -> Result<Self::Port, Self::Error> {
        Err(UnsupportedFunction::new("UART"))
    }
}

impl I2sProvider for NoExposedIo {
    type Stream = UnavailableI2s;
    type Error = UnsupportedFunction;

    fn open_i2s(&self, _request: I2sRequest<'_>) -> Result<Self::Stream, Self::Error> {
        Err(UnsupportedFunction::new("I2S"))
    }
}

impl I2cProvider for NoExposedIo {
    type Bus = UnavailableI2c;
    type Error = UnsupportedFunction;

    fn open_i2c(&self, _request: I2cRequest<'_>) -> Result<Self::Bus, Self::Error> {
        Err(UnsupportedFunction::new("I2C"))
    }
}

impl SpiProvider for NoExposedIo {
    type Bus = UnavailableSpi;
    type Error = UnsupportedFunction;

    fn open_spi(&self, _request: SpiRequest<'_>) -> Result<Self::Bus, Self::Error> {
        Err(UnsupportedFunction::new("SPI"))
    }
}

/// Complete empty Board hardware surface.
pub type NoBoardResources = BoardResources<NoPeripherals, NoExposedIo>;

impl BoardHal for EmptyBoardHal {
    type Bindings = ();
    type Resources = NoBoardResources;
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _bindings: ()) -> BoardHalInitResult<Self> {
        Ok(BoardResources::new(NoPeripherals, NoExposedIo))
    }
}
