//! MIPI DBI display on an eight-bit Intel 8080 parallel bus.
//!
//! The Platform supplies the bus as a [`ParallelBus`]; this implementation
//! adapts it to `mipidsi`'s interface and otherwise drives the panel exactly
//! as `mipi-dbi-display` does, including its reset and backlight handling.
//! The panel's read strobe, when the Board wires it, is driven inactive
//! (high) and left so for the rest of the program.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::ParallelBus;
use barracuda_mipi_dbi_display::{
    DigitalBacklight, MipiDbiDisplay, MipiDbiInitError, initialize_on,
};
use barracuda_peripheral::PeripheralImplementation;
use embedded_hal::{delay::DelayNs, digital::OutputPin};
use mipidsi::{
    interface::{Interface, InterfaceKind, InterfacePixelFormat},
    models::Model,
};

pub use barracuda_mipi_dbi_display::{
    GC9A01, GC9107, ILI9342CRgb565, MipiDbiColorOrder, MipiDbiDisplayConfig, MipiDbiDriverConfig,
    ST7735s, ST7789, Size,
};

/// Bytes staged per bus write while streaming pixels.
const STAGING_BYTES: usize = 512;

/// `mipidsi` interface over a Platform parallel bus.
pub struct ParallelInterface<BUS> {
    bus: BUS,
    staging: [u8; STAGING_BYTES],
}

impl<BUS> ParallelInterface<BUS> {
    /// Wraps the Platform bus.
    #[must_use]
    pub const fn new(bus: BUS) -> Self {
        Self {
            bus,
            staging: [0; STAGING_BYTES],
        }
    }
}

impl<BUS: ParallelBus> Interface for ParallelInterface<BUS> {
    type Word = u8;
    type Error = BUS::Error;

    const KIND: InterfaceKind = InterfaceKind::Parallel8Bit;

    fn send_command(&mut self, command: u8, args: &[u8]) -> Result<(), Self::Error> {
        self.bus.write_command(command, args)
    }

    fn send_pixels<const N: usize>(
        &mut self,
        pixels: impl IntoIterator<Item = [u8; N]>,
    ) -> Result<(), Self::Error> {
        let mut filled = 0;
        for pixel in pixels {
            if filled + N > STAGING_BYTES {
                self.bus.write_data(&self.staging[..filled])?;
                filled = 0;
            }
            self.staging[filled..filled + N].copy_from_slice(&pixel);
            filled += N;
        }
        self.bus.write_data(&self.staging[..filled])
    }

    fn send_repeated_pixel<const N: usize>(
        &mut self,
        pixel: [u8; N],
        count: u32,
    ) -> Result<(), Self::Error> {
        let per_chunk = (STAGING_BYTES / N.max(1)).max(1);
        let chunk_pixels = usize::try_from(count).map_or(per_chunk, |count| count.min(per_chunk));
        for slot in self.staging[..chunk_pixels * N].chunks_exact_mut(N.max(1)) {
            slot.copy_from_slice(&pixel);
        }
        let mut remaining = usize::try_from(count).unwrap_or(usize::MAX);
        while remaining > 0 {
            let pixels = remaining.min(chunk_pixels);
            self.bus.write_data(&self.staging[..pixels * N])?;
            remaining -= pixels;
        }
        Ok(())
    }
}

/// Move-only resources consumed by initialization.
pub struct MipiDbiParallelDisplayBindings<BUS, RD, RST, BL, DELAY> {
    bus: BUS,
    read: Option<RD>,
    reset: Option<RST>,
    backlight: BL,
    delay: DELAY,
}

impl<BUS, RD, RST, BL, DELAY> MipiDbiParallelDisplayBindings<BUS, RD, RST, BL, DELAY> {
    /// Combines the parallel bus, the optional read strobe and reset, the
    /// backlight output, and a delay.
    #[must_use]
    pub const fn new(
        bus: BUS,
        read: Option<RD>,
        reset: Option<RST>,
        backlight: BL,
        delay: DELAY,
    ) -> Self {
        Self {
            bus,
            read,
            reset,
            backlight,
            delay,
        }
    }
}

/// Initialized parallel-bus panel.
pub type MipiDbiParallelDisplay<BUS, M, RST, BL, DELAY> =
    MipiDbiDisplay<ParallelInterface<BUS>, M, RST, DigitalBacklight<BL>, DELAY>;

/// Initialization failure.
#[derive(Debug)]
pub enum MipiDbiParallelInitError<PanelError, ReadError> {
    /// The read strobe could not be held inactive.
    Read(ReadError),
    /// The panel failed to initialize.
    Panel(PanelError),
}

/// Bindings carried only at the type level by the static factory.
type FactoryTypes<BUS, M, RD, RST, BL, DELAY> = PhantomData<fn() -> (BUS, M, RD, RST, BL, DELAY)>;

/// Static factory used by generated Board composition.
pub struct MipiDbiParallelDisplayImplementation<BUS, M, RD, RST, BL, DELAY>(
    FactoryTypes<BUS, M, RD, RST, BL, DELAY>,
);

impl<BUS, M, RD, RST, BL, DELAY> PeripheralImplementation
    for MipiDbiParallelDisplayImplementation<BUS, M, RD, RST, BL, DELAY>
where
    BUS: ParallelBus + 'static,
    M: Model + 'static,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RD: OutputPin + 'static,
    RST: OutputPin + 'static,
    BL: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = MipiDbiParallelDisplayBindings<BUS, RD, RST, BL, DELAY>;
    type Config = MipiDbiDriverConfig<M>;
    type Peripheral = MipiDbiParallelDisplay<BUS, M, RST, BL, DELAY>;
    type Error =
        MipiDbiParallelInitError<MipiDbiInitError<BUS::Error, RST::Error, BL::Error>, RD::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let MipiDbiParallelDisplayBindings {
            bus,
            mut read,
            reset,
            backlight,
            delay,
        } = bindings;
        if let Some(mut read) = read.take() {
            read.set_high().map_err(MipiDbiParallelInitError::Read)?;
            // The panel is only written; its read strobe must stay inactive
            // for as long as the program runs, so its driver is never dropped.
            core::mem::forget(read);
        }
        let backlight = config.digital_backlight(Some(backlight));
        initialize_on(
            ParallelInterface::new(bus),
            reset,
            backlight,
            (),
            delay,
            config,
        )
        .map_err(MipiDbiParallelInitError::Panel)
    }
}

#[cfg(test)]
mod tests;
