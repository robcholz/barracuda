//! MIPI DBI display whose backlight enable is a PI4IOE5V6408 expander output.
//!
//! The panel itself is driven by `mipi-dbi-display`. Its backlight switch is
//! one output of a PI4IOE5V6408 I2C expander that stays private to this
//! implementation, as on the M5Stack StampPLC (expander `0x43`, output 7).

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_mipi_dbi_display::{
    MipiDbiDisplay, MipiDbiDisplayBindings, MipiDbiDisplayImplementation, MipiDbiDriverConfig,
    MipiDbiInitError,
};
use barracuda_peripheral::PeripheralImplementation;
use embedded_hal::{
    delay::DelayNs,
    digital::{self, OutputPin},
    i2c::I2c,
    spi::SpiDevice,
};
use mipidsi::{
    interface::{InterfacePixelFormat, SpiError, SpiInterface},
    models::Model,
};

pub use barracuda_mipi_dbi_display::{
    GC9A01, GC9107, ILI9342CRgb565, MipiDbiColorOrder, MipiDbiDisplayConfig, ST7735s, ST7789, Size,
    take_transfer_buffer,
};

/// One PI4IOE5V6408 output used as a push-pull digital output.
pub struct ExpanderOutput<I2C> {
    i2c: I2C,
    address: u8,
    pin: u8,
}

impl<I2C: I2c> ExpanderOutput<I2C> {
    /// Configures `pin` of the expander at `address` as an output driving
    /// `initial_high`.
    ///
    /// # Errors
    ///
    /// Returns an invalid address or pin, or an I2C register failure.
    pub fn new(
        mut i2c: I2C,
        address: u8,
        pin: u8,
        initial_high: bool,
    ) -> Result<Self, ExpanderError<I2C::Error>> {
        Pi4ioe5v6408::new(&mut i2c, address)?.configure_output(pin, initial_high)?;
        Ok(Self { i2c, address, pin })
    }

    fn drive(&mut self, high: bool) -> Result<(), ExpanderPinError<I2C::Error>> {
        Pi4ioe5v6408::new(&mut self.i2c, self.address)
            .and_then(|mut expander| expander.set_output(self.pin, high))
            .map_err(ExpanderPinError)
    }
}

/// Expander register failure seen through the digital-output contract.
#[derive(Debug)]
pub struct ExpanderPinError<BusError>(pub ExpanderError<BusError>);

impl<BusError: core::fmt::Debug> digital::Error for ExpanderPinError<BusError> {
    fn kind(&self) -> digital::ErrorKind {
        digital::ErrorKind::Other
    }
}

impl<I2C: I2c> digital::ErrorType for ExpanderOutput<I2C> {
    type Error = ExpanderPinError<I2C::Error>;
}

impl<I2C: I2c> OutputPin for ExpanderOutput<I2C> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.drive(false)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.drive(true)
    }
}

/// Where the backlight enable sits on the expander.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpanderBacklight {
    address: u8,
    pin: u8,
    active_high: bool,
}

impl ExpanderBacklight {
    /// Names the expander address, its output, and the output's active level.
    #[must_use]
    pub const fn new(address: u8, pin: u8, active_high: bool) -> Self {
        Self {
            address,
            pin,
            active_high,
        }
    }
}

/// Panel model and options plus the backlight's expander output.
pub struct MipiDbiExpanderDisplayConfig<M> {
    backlight: ExpanderBacklight,
    panel: MipiDbiDriverConfig<M>,
}

impl<M> MipiDbiExpanderDisplayConfig<M> {
    /// Combines the expander backlight with one controller model and its
    /// options. The backlight polarity in `display` is replaced by
    /// `backlight`'s.
    #[must_use]
    pub const fn new(
        backlight: ExpanderBacklight,
        model: M,
        display: MipiDbiDisplayConfig,
    ) -> Self {
        Self {
            backlight,
            panel: MipiDbiDriverConfig::new(
                model,
                display.with_digital_backlight(backlight.active_high),
            ),
        }
    }
}

/// Move-only resources consumed by initialization.
pub struct MipiDbiExpanderDisplayBindings<SPI, DC, RST, I2C, DELAY> {
    spi: SPI,
    dc: DC,
    reset: Option<RST>,
    i2c: I2C,
    delay: DELAY,
    buffer: &'static mut [u8; 512],
}

impl<SPI, DC, RST, I2C, DELAY> MipiDbiExpanderDisplayBindings<SPI, DC, RST, I2C, DELAY> {
    /// Combines the panel's SPI device and outputs with the expander's bus.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        reset: Option<RST>,
        i2c: I2C,
        delay: DELAY,
        buffer: &'static mut [u8; 512],
    ) -> Self {
        Self {
            spi,
            dc,
            reset,
            i2c,
            delay,
            buffer,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum MipiDbiExpanderInitError<PanelError, BusError> {
    /// The backlight output could not be configured on the expander.
    Expander(ExpanderError<BusError>),
    /// The panel failed to initialize.
    Panel(PanelError),
}

/// Initialized display.
pub type MipiDbiExpanderDisplay<SPI, DC, M, RST, I2C, DELAY> =
    MipiDbiDisplay<SpiInterface<'static, SPI, DC>, M, RST, ExpanderOutput<I2C>, DELAY>;

/// Static factory used by generated Board composition.
pub struct MipiDbiExpanderDisplayImplementation<SPI, DC, M, RST, I2C, DELAY>(
    FactoryTypes<SPI, DC, M, RST, I2C, DELAY>,
);

/// Bindings carried only at the type level by the static factory.
type FactoryTypes<SPI, DC, M, RST, I2C, DELAY> = PhantomData<fn() -> (SPI, DC, M, RST, I2C, DELAY)>;

impl<SPI, DC, M, RST, I2C, DELAY> PeripheralImplementation
    for MipiDbiExpanderDisplayImplementation<SPI, DC, M, RST, I2C, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    M: Model + 'static,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    DC::Error: 'static,
    RST::Error: 'static,
    I2C::Error: 'static,
{
    type Bindings = MipiDbiExpanderDisplayBindings<SPI, DC, RST, I2C, DELAY>;
    type Config = MipiDbiExpanderDisplayConfig<M>;
    type Peripheral = MipiDbiExpanderDisplay<SPI, DC, M, RST, I2C, DELAY>;
    type Error = MipiDbiExpanderInitError<
        MipiDbiInitError<SpiError<SPI::Error, DC::Error>, RST::Error, ExpanderPinError<I2C::Error>>,
        I2C::Error,
    >;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let backlight = config.backlight;
        // Start with the backlight off; the panel turns it on once initialized.
        let output = ExpanderOutput::new(
            bindings.i2c,
            backlight.address,
            backlight.pin,
            !backlight.active_high,
        )
        .map_err(MipiDbiExpanderInitError::Expander)?;
        MipiDbiDisplayImplementation::<SPI, DC, M, RST, ExpanderOutput<I2C>, DELAY, 512>::initialize(
            MipiDbiDisplayBindings::new(
                bindings.spi,
                bindings.dc,
                bindings.reset,
                Some(output),
                bindings.delay,
                bindings.buffer,
            ),
            config.panel,
        )
        .await
        .map_err(MipiDbiExpanderInitError::Panel)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use embedded_hal::{
        digital::OutputPin,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::ExpanderOutput;

    /// Register file of one PI4IOE5V6408 that records writes.
    #[derive(Default)]
    struct Bus {
        registers: [u8; 0x12],
        writes: Vec<(u8, u8)>,
    }

    impl ErrorType for Bus {
        type Error = core::convert::Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut selected = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => selected = *register,
                    Operation::Write([register, value]) => {
                        self.registers[usize::from(*register)] = *value;
                        self.writes.push((*register, *value));
                    }
                    Operation::Read([value]) => *value = self.registers[usize::from(selected)],
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn backlight_output_is_configured_off_then_switched() {
        let mut bus = Bus::default();
        bus.registers[0x07] = 0xff;
        {
            let mut output = ExpanderOutput::new(&mut bus, 0x43, 7, true).expect("configure");
            output.set_low().expect("drive low");
        }
        // Output latch first, then direction, then high impedance released.
        assert_eq!(
            bus.writes,
            [(0x05, 0x80), (0x03, 0x80), (0x07, 0x7f), (0x05, 0x00)]
        );
    }
}
