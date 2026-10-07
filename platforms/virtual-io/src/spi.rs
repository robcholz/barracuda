//! Virtual SPI controller exposed through the `embedded-hal` contracts.

use core::fmt;

use embedded_hal::spi::ErrorKind;

use crate::{
    device::{DeviceContext, RegisterRangeError},
    hardware::{FaultKind, VirtualHardware},
};

/// Behaviour of the device driven by one SPI controller.
///
/// Every bus operation is one full-duplex transfer: `write` holds the MOSI
/// bytes and the model fills `read` with the MISO bytes (pre-filled with FFh,
/// an undriven line). The context's time is the start of the transfer; the
/// model derives bit timing from `frequency_hz`.
pub trait SpiDevice: Send {
    /// Short model name reported by the control interface.
    fn model(&self) -> &'static str;

    /// Called once when the device is attached (its power-on time).
    fn attached(&mut self, _context: &mut DeviceContext<'_>) {}

    /// Receives one transfer.
    fn transfer(
        &mut self,
        context: &mut DeviceContext<'_>,
        frequency_hz: u32,
        write: &[u8],
        read: &mut [u8],
    );

    /// Copies model-defined bytes without bus side effects.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterRangeError`] when the range is outside the model.
    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError>;
}

/// Failure of one virtual SPI transfer, produced by an injected fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualSpiError {
    fault: FaultKind,
}

impl VirtualSpiError {
    pub(crate) const fn new(fault: FaultKind) -> Self {
        Self { fault }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self.fault {
            FaultKind::Nack => "nack",
            FaultKind::ArbitrationLoss => "arbitration-loss",
            FaultKind::Timeout => "timeout",
            FaultKind::BusError => "bus-error",
        }
    }
}

impl fmt::Display for VirtualSpiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SPI transfer failed: {}", self.label())
    }
}

impl core::error::Error for VirtualSpiError {}

impl embedded_hal::spi::Error for VirtualSpiError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// One SPI controller of a [`VirtualHardware`] model at a fixed clock.
///
/// It implements the blocking and async `embedded-hal` `SpiBus` traits.
#[derive(Clone, Debug)]
pub struct VirtualSpiBus {
    hardware: VirtualHardware,
    bus: usize,
    frequency_hz: u32,
}

impl VirtualSpiBus {
    pub(crate) const fn new(hardware: VirtualHardware, bus: usize, frequency_hz: u32) -> Self {
        Self {
            hardware,
            bus,
            frequency_hz,
        }
    }

    fn transfer_bytes(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), VirtualSpiError> {
        self.hardware
            .spi_transfer(self.bus, self.frequency_hz, write, read)
    }

    fn exchange(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), VirtualSpiError> {
        // embedded-hal: the longer buffer sets the length; missing write
        // bytes are 00h and extra read bytes are discarded.
        let length = read.len().max(write.len());
        let mut out = vec![0; length];
        out[..write.len()].copy_from_slice(write);
        let mut input = vec![0; length];
        self.transfer_bytes(&out, &mut input)?;
        let count = read.len();
        read.copy_from_slice(&input[..count]);
        Ok(())
    }
}

impl embedded_hal::spi::ErrorType for VirtualSpiBus {
    type Error = VirtualSpiError;
}

impl embedded_hal::spi::SpiBus<u8> for VirtualSpiBus {
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.exchange(words, &[])
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        self.exchange(&mut [], words)
    }

    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        self.exchange(read, write)
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        let write = words.to_vec();
        self.exchange(words, &write)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal_async::spi::SpiBus<u8> for VirtualSpiBus {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.exchange(words, &[])
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        self.exchange(&mut [], words)
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        self.exchange(read, write)
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        let write = words.to_vec();
        self.exchange(words, &write)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::spi::SpiBus as _;

    use super::*;
    use crate::{Clock, EventDetail};

    struct Echo;

    impl SpiDevice for Echo {
        fn model(&self) -> &'static str {
            "echo"
        }

        fn transfer(
            &mut self,
            _context: &mut DeviceContext<'_>,
            _frequency_hz: u32,
            write: &[u8],
            read: &mut [u8],
        ) {
            read.copy_from_slice(write);
        }

        fn peek(&self, offset: usize, _buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
            Err(RegisterRangeError { offset, length: 0 })
        }
    }

    #[test]
    fn transfers_are_full_duplex_logged_and_faultable() {
        let hardware = VirtualHardware::new(&[], &[], Clock::manual()).with_spi(&["SPI2"]);
        let mut bus = hardware.spi_bus("SPI2", 1_000_000).expect("bus");
        let mut read = [0; 2];
        bus.transfer(&mut read, &[1, 2]).expect("no device");
        assert_eq!(read, [0xff, 0xff], "MISO floats high");
        hardware.attach_spi("SPI2", Box::new(Echo)).expect("attach");
        bus.transfer(&mut read, &[3]).expect("echo");
        assert_eq!(read, [3, 0]);
        hardware
            .inject_fault("SPI2", None, FaultKind::Timeout, true)
            .expect("fault");
        assert!(hardware
            .inject_fault("SPI2", Some(1), FaultKind::Timeout, true)
            .is_err());
        assert_eq!(
            bus.write(&[9]).map_err(|error| error.label()),
            Err("timeout")
        );
        bus.write(&[9]).expect("fault fired once");
        let events = hardware.events(0);
        assert!(matches!(
            &events[1].detail,
            EventDetail::Spi { write, read, .. } if write == "0300" && read == "0300"
        ));
        assert!(matches!(
            events[2].detail,
            EventDetail::Spi {
                result: "timeout",
                injected: true,
                ..
            }
        ));
    }
}
