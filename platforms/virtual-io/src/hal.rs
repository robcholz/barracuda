//! Host Platform HAL surface over the process-wide virtual hardware.
//!
//! The Linux and macOS Platforms re-export this module as their `hal`
//! module. Generated Board composition then names virtual pins and
//! controllers exactly like chip tokens: `pin_binding_type!(GPIO0)`,
//! `controller_binding_type!(I2C0)`, and the fields of
//! [`VirtualPeripherals`], the host's virtual peripheral singleton.

use core::convert::Infallible;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

use barracuda_board_hal::{
    audio::PcmFormat, RuntimeAnalogPlatform, RuntimeI2sPlatform, RuntimePlatform,
    RuntimePwmPlatform, RuntimeUartPlatform, SharedI2cBus, UartConfig, UnavailableAnalogInput,
    UnavailableAnalogOutput, UnavailableI2s, UnavailablePwm, UnavailableSpi, UnavailableUart,
    UnsupportedFunction,
};
use embedded_hal::spi::Mode;

use crate::{
    clock::Clock, control, gpio::VirtualDigitalPin, hardware::VirtualHardware, i2c::VirtualI2cBus,
};

/// Environment variable naming the loopback address of the control server.
pub const ADDRESS_VARIABLE: &str = "BARRACUDA_VIRTUAL_IO_ADDR";

/// Chip-native names of the virtual pins, in index order.
pub const PIN_NAMES: [&str; 16] = [
    "GPIO0", "GPIO1", "GPIO2", "GPIO3", "GPIO4", "GPIO5", "GPIO6", "GPIO7", "GPIO8", "GPIO9",
    "GPIO10", "GPIO11", "GPIO12", "GPIO13", "GPIO14", "GPIO15",
];

/// Names of the virtual I2C controllers, in index order.
pub const I2C_CONTROLLER_NAMES: [&str; 2] = ["I2C0", "I2C1"];

static HARDWARE: OnceLock<VirtualHardware> = OnceLock::new();
static CONTROL_ADDRESS: OnceLock<SocketAddr> = OnceLock::new();
static TAKEN: AtomicBool = AtomicBool::new(false);

/// The process-wide virtual hardware behind the host Platform.
#[must_use]
pub fn hardware() -> &'static VirtualHardware {
    HARDWARE.get_or_init(|| VirtualHardware::new(&PIN_NAMES, &I2C_CONTROLLER_NAMES, Clock::real()))
}

/// The virtual peripherals could not be started.
#[derive(Debug, thiserror::Error)]
pub enum VirtualIoStartError {
    /// The singleton was already taken in this process.
    #[error("the virtual peripherals were already taken")]
    AlreadyTaken,
    /// The control address is not configured.
    #[error(
        "{ADDRESS_VARIABLE} is not set; set it to a loopback address such as 127.0.0.1:7878 \
         for the virtual peripherals manager"
    )]
    MissingAddress,
    /// The control address is not a socket address.
    #[error("{ADDRESS_VARIABLE}={value:?} is not a socket address such as 127.0.0.1:7878")]
    InvalidAddress {
        /// Configured value.
        value: String,
    },
    /// The control address is not on the loopback interface.
    #[error("{ADDRESS_VARIABLE}={address} must be a loopback address")]
    NotLoopback {
        /// Configured address.
        address: SocketAddr,
    },
    /// The control address could not be bound.
    #[error("{ADDRESS_VARIABLE}={address} cannot be bound: {source}")]
    Bind {
        /// Configured address.
        address: SocketAddr,
        /// Bind failure.
        source: std::io::Error,
    },
}

/// Parses the control address configured in [`ADDRESS_VARIABLE`].
///
/// # Errors
///
/// Returns an error when the value is missing, unparsable, or not loopback.
pub fn control_address(value: Option<&str>) -> Result<SocketAddr, VirtualIoStartError> {
    let value = value.ok_or(VirtualIoStartError::MissingAddress)?;
    let address: SocketAddr =
        value
            .trim()
            .parse()
            .map_err(|_error| VirtualIoStartError::InvalidAddress {
                value: String::from(value),
            })?;
    if address.ip().is_loopback() {
        Ok(address)
    } else {
        Err(VirtualIoStartError::NotLoopback { address })
    }
}

macro_rules! virtual_peripherals {
    (pins: [$($pin:ident = $pin_index:literal),* $(,)?], i2c: [$($bus:ident = $bus_index:literal),* $(,)?]) => {
        /// Move-only tokens of every virtual pin and controller.
        ///
        /// This is the host counterpart of a chip's peripheral singleton:
        /// the Platform entry takes it once and generated Board bindings
        /// move the declared fields out of it.
        #[allow(non_snake_case)]
        #[derive(Debug)]
        pub struct VirtualPeripherals {
            $(
                #[doc = concat!("Virtual pin `", stringify!($pin), "`.")]
                pub $pin: VirtualPinToken,
            )*
            $(
                #[doc = concat!("Virtual I2C controller `", stringify!($bus), "`.")]
                pub $bus: VirtualI2cController,
            )*
        }

        impl VirtualPeripherals {
            fn from_hardware(hardware: &VirtualHardware) -> Self {
                Self {
                    $($pin: VirtualPinToken { hardware: hardware.clone(), pin: $pin_index },)*
                    $($bus: VirtualI2cController { hardware: hardware.clone(), bus: $bus_index },)*
                }
            }
        }
    };
}

virtual_peripherals!(
    pins: [
        GPIO0 = 0, GPIO1 = 1, GPIO2 = 2, GPIO3 = 3, GPIO4 = 4, GPIO5 = 5, GPIO6 = 6, GPIO7 = 7,
        GPIO8 = 8, GPIO9 = 9, GPIO10 = 10, GPIO11 = 11, GPIO12 = 12, GPIO13 = 13, GPIO14 = 14,
        GPIO15 = 15,
    ],
    i2c: [I2C0 = 0, I2C1 = 1]
);

impl VirtualPeripherals {
    /// Takes the process-wide virtual peripherals and starts their manager.
    ///
    /// The control server listens on the loopback address named by
    /// [`ADDRESS_VARIABLE`]. A host System has no virtual hardware without
    /// it, so a missing or invalid address is a startup error.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualIoStartError`] when the singleton was already taken
    /// or the control address is missing, invalid, or cannot be bound.
    pub fn take() -> Result<Self, VirtualIoStartError> {
        if TAKEN.swap(true, Ordering::AcqRel) {
            return Err(VirtualIoStartError::AlreadyTaken);
        }
        let value = std::env::var(ADDRESS_VARIABLE).ok();
        let address = control_address(value.as_deref())?;
        let hardware = hardware();
        let bound = control::start(hardware.clone(), address)
            .map_err(|source| VirtualIoStartError::Bind { address, source })?;
        let _first = CONTROL_ADDRESS.set(bound);
        Ok(Self::from_hardware(hardware))
    }
}

/// Move-only token of one virtual pin.
#[derive(Debug)]
pub struct VirtualPinToken {
    hardware: VirtualHardware,
    pin: usize,
}

/// Move-only token of one virtual I2C controller.
#[derive(Debug)]
pub struct VirtualI2cController {
    hardware: VirtualHardware,
    bus: usize,
}

/// A virtual I2C controller rejected its configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VirtualI2cConfigError {
    /// A zero bus clock was requested.
    #[error("I2C frequency must be nonzero")]
    ZeroFrequency,
}

/// Platform adapter used by generated runtime I/O composition.
#[derive(Debug)]
pub struct VirtualRuntime;

impl RuntimePlatform for VirtualRuntime {
    type PinToken = VirtualPinToken;
    type DigitalPin = VirtualDigitalPin;
    type I2cController = VirtualI2cController;
    type I2cBus = VirtualI2cBus;
    type I2cError = VirtualI2cConfigError;
    type SpiController = Infallible;
    type SpiBus = UnavailableSpi;
    type SpiError = UnsupportedFunction;
    type UartController = Infallible;
    type AdcResource = Infallible;
    type PwmResource = Infallible;
    type I2sResource = Infallible;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        VirtualDigitalPin::new(pin.hardware, pin.pin)
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
        if frequency_hz == 0 {
            return Err(VirtualI2cConfigError::ZeroFrequency);
        }
        controller
            .hardware
            .claim_i2c(controller.bus, scl.pin, sda.pin, frequency_hz);
        Ok(VirtualI2cBus::new(controller.hardware, controller.bus))
    }

    fn supports_spi(
        controller: &Self::SpiController,
        _sck: &Self::PinToken,
        _mosi: Option<&Self::PinToken>,
        _miso: Option<&Self::PinToken>,
    ) -> bool {
        match *controller {}
    }

    fn spi(
        controller: Self::SpiController,
        _sck: Self::PinToken,
        _mosi: Option<Self::PinToken>,
        _miso: Option<Self::PinToken>,
        _frequency_hz: u32,
        _mode: Mode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        match controller {}
    }
}

impl RuntimeAnalogPlatform for VirtualRuntime {
    type AnalogInput = UnavailableAnalogInput;
    type AnalogOutput = UnavailableAnalogOutput;
    type AnalogError = UnsupportedFunction;

    fn supports_analog_input(resource: &Self::AdcResource, _pin: &Self::PinToken) -> bool {
        match *resource {}
    }

    fn supports_analog_output(_pin: &Self::PinToken) -> bool {
        false
    }

    fn analog_input(
        resource: Self::AdcResource,
        _pin: Self::PinToken,
    ) -> Result<Self::AnalogInput, Self::AnalogError> {
        match resource {}
    }

    fn analog_output(_pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Err(UnsupportedFunction::new("analog output"))
    }
}

impl RuntimePwmPlatform for VirtualRuntime {
    type Pwm = UnavailablePwm;
    type PwmError = UnsupportedFunction;

    fn supports_pwm(resource: &Self::PwmResource, _pin: &Self::PinToken) -> bool {
        match *resource {}
    }

    fn pwm(
        resource: Self::PwmResource,
        _pin: Self::PinToken,
        _frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        match resource {}
    }
}

impl RuntimeUartPlatform for VirtualRuntime {
    type Uart = UnavailableUart;
    type UartError = UnsupportedFunction;

    fn supports_uart(
        controller: &Self::UartController,
        _tx: Option<&Self::PinToken>,
        _rx: Option<&Self::PinToken>,
    ) -> bool {
        match *controller {}
    }

    fn uart(
        controller: Self::UartController,
        _tx: Option<Self::PinToken>,
        _rx: Option<Self::PinToken>,
        _config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError> {
        match controller {}
    }
}

impl RuntimeI2sPlatform for VirtualRuntime {
    type I2s = UnavailableI2s;
    type I2sError = UnsupportedFunction;

    fn supports_i2s(
        resource: &Self::I2sResource,
        _bclk: &Self::PinToken,
        _ws: &Self::PinToken,
        _dout: Option<&Self::PinToken>,
        _din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        match *resource {}
    }

    fn i2s(
        resource: Self::I2sResource,
        _bclk: Self::PinToken,
        _ws: Self::PinToken,
        _dout: Option<Self::PinToken>,
        _din: Option<Self::PinToken>,
        _mclk: Option<Self::PinToken>,
        _format: PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        match resource {}
    }
}

/// Generated exposed-I/O owner specialized to the virtual hardware.
pub type RuntimeIo<
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
> = barracuda_board_hal::RuntimeIo<VirtualRuntime, P, I, S, U, A, W, T>;

/// Constructs the selected Board's unified runtime I/O owner.
///
/// It gives every exposed virtual pin its Board name in the manager.
#[must_use]
pub fn runtime_io<const P: usize, const I: usize>(
    pins: [(&'static str, VirtualPinToken); P],
    i2c: [VirtualI2cController; I],
    spi: [Infallible; 0],
    uart: [Infallible; 0],
    adc: [Infallible; 0],
    pwm: [Infallible; 0],
    i2s: [Infallible; 0],
) -> RuntimeIo<P, I, 0, 0, 0, 0, 0> {
    for (name, token) in &pins {
        token.hardware.name_pin(token.pin, name);
    }
    if let Some(address) = CONTROL_ADDRESS.get() {
        log::info!("virtual peripherals manager listening on {address}");
    }
    RuntimeIo::new_with_resources(pins, i2c, spi, uart, adc, pwm, i2s)
}

/// Keeps one virtual pin token for the runtime owner.
#[must_use]
pub const fn runtime_pin(pin: VirtualPinToken) -> VirtualPinToken {
    pin
}

/// Keeps one virtual I2C controller token for the runtime owner.
#[must_use]
pub const fn runtime_i2c_controller(controller: VirtualI2cController) -> VirtualI2cController {
    controller
}

/// Blocking virtual I2C bus consumed by statically selected peripheral implementations.
pub type I2cBus = VirtualI2cBus;
/// Static owner of one virtual I2C bus shared by peripheral implementations.
pub type I2cBusManager = SharedI2cBus<I2cBus>;
/// Blocking I2C view handed to one peripheral implementation.
pub type I2cDevice = embedded_hal_bus::i2c::CriticalSectionDevice<'static, I2cBus>;
/// Virtual I2C construction failure.
pub type I2cConfigError = VirtualI2cConfigError;
/// Delay handed to peripheral implementations.
pub type PeripheralDelay = embassy_time::Delay;

/// Constructs a virtual I2C bus for a statically selected peripheral implementation.
///
/// # Errors
///
/// Returns [`VirtualI2cConfigError::ZeroFrequency`] for a zero bus clock.
pub fn i2c_device(
    controller: VirtualI2cController,
    scl: VirtualPinToken,
    sda: VirtualPinToken,
    frequency_hz: u32,
) -> Result<I2cBus, I2cConfigError> {
    VirtualRuntime::i2c(controller, scl, sda, frequency_hz)
}

/// Returns the delay handed to peripheral implementations.
#[must_use]
pub const fn delay() -> PeripheralDelay {
    embassy_time::Delay
}

#[doc(hidden)]
#[must_use]
pub fn __leak_i2c_bus_manager(bus: I2cBus) -> &'static I2cBusManager {
    Box::leak(Box::new(I2cBusManager::new(bus)))
}

/// Allocates one shared virtual I2C bus owner at the Board call site.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_virtual_io_i2c_bus_manager {
    ($bus:expr) => {
        $crate::hal::__leak_i2c_bus_manager($bus)
    };
}

/// Resolves a Board-selected virtual pin name to its token type.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_virtual_io_pin_binding_type {
    ($pin:ident) => {
        $crate::hal::VirtualPinToken
    };
}

/// Resolves a Platform-owned virtual controller name to its token type.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_virtual_io_controller_binding_type {
    ($controller:ident) => {
        $crate::hal::VirtualI2cController
    };
}

#[doc(hidden)]
pub use crate::__barracuda_virtual_io_controller_binding_type as controller_binding_type;
#[doc(hidden)]
pub use crate::__barracuda_virtual_io_i2c_bus_manager as i2c_bus_manager;
#[doc(hidden)]
pub use crate::__barracuda_virtual_io_pin_binding_type as pin_binding_type;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_board_hal::{
        ConfigurableDigitalPin as _, DigitalLevel, DigitalProvider as _, I2cProvider as _,
        I2cRequest, OutputConfig, OutputDrive,
    };
    use embedded_hal::digital::OutputPin as _;
    use embedded_hal_async::i2c::I2c as _;

    use super::*;
    use crate::{device::RegisterDevice, hardware::PinMode};

    #[test]
    fn the_control_address_must_be_a_loopback_socket_address() {
        assert!(matches!(
            control_address(None),
            Err(VirtualIoStartError::MissingAddress)
        ));
        assert!(matches!(
            control_address(Some("localhost")),
            Err(VirtualIoStartError::InvalidAddress { .. })
        ));
        assert!(matches!(
            control_address(Some("0.0.0.0:7878")),
            Err(VirtualIoStartError::NotLoopback { .. })
        ));
        assert_eq!(
            control_address(Some("127.0.0.1:7878"))
                .expect("loopback")
                .port(),
            7878
        );
        assert!(control_address(Some("[::1]:0")).is_ok());
        let message = VirtualIoStartError::MissingAddress.to_string();
        assert!(message.contains(ADDRESS_VARIABLE), "{message}");
    }

    #[test]
    fn generated_composition_exposes_named_pins_and_controllers() {
        let hardware = VirtualHardware::new(&PIN_NAMES, &I2C_CONTROLLER_NAMES, Clock::manual());
        let peripherals = VirtualPeripherals::from_hardware(&hardware);
        let io = runtime_io(
            [
                ("vio-0", runtime_pin(peripherals.GPIO0)),
                ("vio-1", runtime_pin(peripherals.GPIO1)),
                ("vio-2", runtime_pin(peripherals.GPIO2)),
            ],
            [runtime_i2c_controller(peripherals.I2C0)],
            [],
            [],
            [],
            [],
            [],
        );
        let mut pin = io.acquire_digital("vio-2").expect("digital pin");
        pin.configure_output(OutputConfig {
            initial: DigitalLevel::Low,
            drive: OutputDrive::PushPull,
        })
        .expect("output");
        pin.set_high().expect("high");
        let snapshot = hardware.pin("vio-2").expect("pin");
        assert_eq!(snapshot.mode, PinMode::Output);
        assert!(snapshot.level);
        assert!(
            io.acquire_digital("vio-2").is_err(),
            "a pin is claimed once"
        );

        hardware
            .attach("I2C0", 0x48, Box::new(RegisterDevice::new()))
            .expect("attach");
        let mut bus = io
            .open_i2c(I2cRequest {
                scl: "vio-0",
                sda: "vio-1",
                frequency_hz: 100_000,
            })
            .expect("I2C bus");
        embassy_futures::block_on(bus.write(0x48, &[0x01, 0x7f])).expect("write");
        assert_eq!(
            hardware.read_registers("I2C0", 0x48, 1, 1).expect("peek"),
            [0x7f]
        );
        let buses = hardware.buses();
        assert_eq!(buses[0].scl.as_deref(), Some("vio-0"));
        assert_eq!(buses[0].frequency_hz, Some(100_000));
        assert_eq!(
            hardware.pin("vio-1").expect("pin").function.as_deref(),
            Some("I2C0 SDA")
        );
        assert!(
            io.open_i2c(I2cRequest {
                scl: "vio-0",
                sda: "vio-1",
                frequency_hz: 100_000,
            })
            .is_err(),
            "the controller and pins stay claimed"
        );
    }

    #[test]
    fn a_zero_i2c_clock_is_rejected() {
        let hardware = VirtualHardware::new(&PIN_NAMES, &I2C_CONTROLLER_NAMES, Clock::manual());
        let peripherals = VirtualPeripherals::from_hardware(&hardware);
        assert_eq!(
            i2c_device(peripherals.I2C1, peripherals.GPIO8, peripherals.GPIO9, 0).err(),
            Some(VirtualI2cConfigError::ZeroFrequency)
        );
    }
}
