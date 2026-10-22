//! 24Cxx-class I2C serial EEPROM implementation.
//!
//! The memory is identified by its density, which the Board names: a 24Cxx
//! has no identification register. Start-up reads the first byte, so a
//! missing chip fails the Board at boot instead of at first use.
//!
//! A write goes out one page at a time; after each page the implementation
//! polls the chip's acknowledge, once a millisecond, until the chip has
//! programmed it. AT24C02 and FM24C02 data sheets give a 5 ms write cycle;
//! a page still unacknowledged after [`WRITE_CYCLE_LIMIT_MS`] fails the
//! write.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_at24c::{At24c, Error as ChipError};
use barracuda_peripheral::{
    PeripheralImplementation,
    eeprom::{Eeprom, EepromGeometry},
};
use embassy_time::Timer;
use embedded_hal::i2c::I2c;

pub use barracuda_driver_at24c::Variant;

/// Longest a page write may take before it is reported as failed: four
/// times the 5 ms the data sheets allow.
pub const WRITE_CYCLE_LIMIT_MS: u32 = 20;

/// Board-owned EEPROM address and density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At24cEepromConfig {
    address: u8,
    variant: Variant,
}

impl At24cEepromConfig {
    /// Creates configuration for a `variant` part at `address`.
    #[must_use]
    pub const fn new(address: u8, variant: Variant) -> Self {
        Self { address, variant }
    }
}

/// EEPROM failure.
#[derive(Debug, PartialEq, Eq)]
pub enum At24cEepromError<E> {
    /// The address is not a valid device address for the density.
    InvalidAddress(u8),
    /// The range does not lie within the memory.
    OutOfRange,
    /// An I2C transfer failed.
    Bus(E),
    /// The chip did not finish programming a page in time.
    WriteTimeout,
}

impl<E> From<ChipError<E>> for At24cEepromError<E> {
    fn from(error: ChipError<E>) -> Self {
        match error {
            ChipError::Bus(error) => Self::Bus(error),
            // The implementation splits writes at page boundaries, so a
            // crossing page can only come from a range past the end.
            ChipError::OutOfRange | ChipError::CrossesPage => Self::OutOfRange,
        }
    }
}

/// Initialized EEPROM.
pub struct At24cEeprom<I2C> {
    i2c: I2C,
    chip: At24c,
}

/// Static factory used by generated Board composition.
pub struct At24cEepromImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for At24cEepromImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = At24cEepromConfig;
    type Peripheral = At24cEeprom<I2C>;
    type Error = At24cEepromError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let chip = At24c::new(config.address, config.variant)
            .ok_or(At24cEepromError::InvalidAddress(config.address))?;
        let mut eeprom = At24cEeprom { i2c, chip };
        let mut probe = [0u8; 1];
        eeprom.chip.read(&mut eeprom.i2c, 0, &mut probe)?;
        Ok(eeprom)
    }
}

impl<I2C: I2c> At24cEeprom<I2C> {
    /// Polls the chip until it acknowledges again after a page write.
    async fn wait_programmed(&mut self) -> Result<(), At24cEepromError<I2C::Error>> {
        for _ in 0..WRITE_CYCLE_LIMIT_MS {
            Timer::after_millis(1).await;
            if self
                .chip
                .is_ready(&mut self.i2c)
                .map_err(At24cEepromError::Bus)?
            {
                return Ok(());
            }
        }
        Err(At24cEepromError::WriteTimeout)
    }
}

impl<I2C: I2c> Eeprom for At24cEeprom<I2C> {
    type Error = At24cEepromError<I2C::Error>;

    fn geometry(&self) -> EepromGeometry {
        let variant = self.chip.variant();
        EepromGeometry::new(variant.capacity(), variant.page_size())
    }

    async fn read(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Self::Error> {
        if buffer.is_empty() {
            return Ok(());
        }
        self.chip
            .read(&mut self.i2c, offset, buffer)
            .map_err(Into::into)
    }

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        if !self.geometry().contains(offset, data.len()) {
            return Err(At24cEepromError::OutOfRange);
        }
        let mut offset = offset;
        let mut rest = data;
        while !rest.is_empty() {
            let room = usize::try_from(self.chip.page_end(offset).saturating_sub(offset))
                .unwrap_or(usize::MAX);
            let (page, remaining) = rest.split_at(room.min(rest.len()));
            self.chip.write_page(&mut self.i2c, offset, page)?;
            self.wait_programmed().await?;
            offset = offset.saturating_add(u32::try_from(page.len()).unwrap_or(u32::MAX));
            rest = remaining;
        }
        Ok(())
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

    use std::vec::Vec;

    use barracuda_peripheral::{PeripheralImplementation, eeprom::Eeprom};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    use super::{At24cEepromConfig, At24cEepromError, At24cEepromImplementation, Variant};

    #[derive(Debug, PartialEq, Eq)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> ErrorKind {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        }
    }

    /// A 24C02 at 50h that stays busy for `cycle_polls` address phases
    /// after each page write and wraps writes within their page.
    struct Fm24c02 {
        cells: [u8; 256],
        pointer: usize,
        busy: u8,
        cycle_polls: u8,
        page_writes: Vec<(u8, usize)>,
    }

    impl Fm24c02 {
        fn new(cycle_polls: u8) -> Self {
            Self {
                cells: [0xff; 256],
                pointer: 0,
                busy: 0,
                cycle_polls,
                page_writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Fm24c02 {
        type Error = Nack;
    }

    impl I2c for Fm24c02 {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address != 0x50 {
                return Err(Nack);
            }
            if self.busy > 0 {
                self.busy -= 1;
                return Err(Nack);
            }
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.pointer = usize::from(bytes[0]);
                        let data = &bytes[1..];
                        if !data.is_empty() {
                            self.page_writes.push((bytes[0], data.len()));
                            let start = self.pointer - self.pointer % 8;
                            for (index, value) in data.iter().enumerate() {
                                self.cells[start + (self.pointer - start + index) % 8] = *value;
                            }
                            self.busy = self.cycle_polls;
                        }
                    }
                    Operation::Read(buffer) => {
                        for value in buffer.iter_mut() {
                            *value = self.cells[self.pointer];
                            self.pointer = (self.pointer + 1) % 256;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn start(chip: Fm24c02) -> super::At24cEeprom<Fm24c02> {
        block_on(At24cEepromImplementation::initialize(
            chip,
            At24cEepromConfig::new(0x50, Variant::C02),
        ))
        .expect("start")
    }

    #[test]
    fn a_missing_chip_fails_at_start() {
        let result = block_on(At24cEepromImplementation::<Fm24c02>::initialize(
            Fm24c02::new(0),
            At24cEepromConfig::new(0x51, Variant::C02),
        ));
        assert!(matches!(result, Err(At24cEepromError::Bus(Nack))));
    }

    #[test]
    fn reports_the_24c02_geometry() {
        let eeprom = start(Fm24c02::new(0));
        let geometry = eeprom.geometry();
        assert_eq!((geometry.capacity(), geometry.page_size()), (256, 8));
    }

    #[test]
    fn splits_a_write_at_page_boundaries_and_waits_out_each_cycle() {
        let mut eeprom = start(Fm24c02::new(3));
        let data: Vec<u8> = (0..20).collect();
        block_on(eeprom.write(5, &data)).expect("write");
        assert_eq!(eeprom.i2c.page_writes, [(5, 3), (8, 8), (16, 8), (24, 1)]);
        let mut back = [0u8; 20];
        block_on(eeprom.read(5, &mut back)).expect("read");
        assert_eq!(back.as_slice(), data.as_slice());
    }

    #[test]
    fn refuses_a_range_past_the_end_without_writing() {
        let mut eeprom = start(Fm24c02::new(0));
        assert_eq!(
            block_on(eeprom.write(250, &[0; 7])),
            Err(At24cEepromError::OutOfRange)
        );
        assert!(eeprom.i2c.page_writes.is_empty());
        let mut buffer = [0u8; 2];
        assert_eq!(
            block_on(eeprom.read(255, &mut buffer)),
            Err(At24cEepromError::OutOfRange)
        );
    }

    #[test]
    fn a_chip_that_never_finishes_times_out() {
        let mut eeprom = start(Fm24c02::new(u8::MAX));
        assert_eq!(
            block_on(eeprom.write(0, &[1])),
            Err(At24cEepromError::WriteTimeout)
        );
    }
}
