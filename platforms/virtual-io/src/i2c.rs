//! Virtual I2C controller exposed through the `embedded-hal` contracts.

use core::fmt;

use embedded_hal::i2c::{ErrorKind, NoAcknowledgeSource, Operation, SevenBitAddress};

use crate::hardware::VirtualHardware;

/// Failure of one virtual I2C transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualI2cError {
    /// The address or a data byte was not acknowledged.
    NoAcknowledge(NoAcknowledgeSource),
    /// Another controller won arbitration.
    ArbitrationLoss,
    /// The transaction did not complete in time.
    Timeout,
    /// A misplaced START or STOP was detected.
    Bus,
}

impl VirtualI2cError {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NoAcknowledge(_) => "nack",
            Self::ArbitrationLoss => "arbitration-loss",
            Self::Timeout => "timeout",
            Self::Bus => "bus-error",
        }
    }
}

impl fmt::Display for VirtualI2cError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAcknowledge(source) => write!(formatter, "I2C NACK: {source}"),
            Self::ArbitrationLoss => formatter.write_str("I2C arbitration lost"),
            Self::Timeout => formatter.write_str("I2C transaction timed out"),
            Self::Bus => formatter.write_str("I2C bus error"),
        }
    }
}

impl core::error::Error for VirtualI2cError {}

impl embedded_hal::i2c::Error for VirtualI2cError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::NoAcknowledge(source) => ErrorKind::NoAcknowledge(*source),
            Self::ArbitrationLoss => ErrorKind::ArbitrationLoss,
            Self::Timeout => ErrorKind::Other,
            Self::Bus => ErrorKind::Bus,
        }
    }
}

/// One controller of a [`VirtualHardware`] model.
///
/// It implements both the blocking and the async `embedded-hal` I2C traits,
/// so a chip driver under test and a VM I2C handle use the same bus.
#[derive(Clone, Debug)]
pub struct VirtualI2cBus {
    hardware: VirtualHardware,
    bus: usize,
}

impl VirtualI2cBus {
    pub(crate) const fn new(hardware: VirtualHardware, bus: usize) -> Self {
        Self { hardware, bus }
    }
}

impl embedded_hal::i2c::ErrorType for VirtualI2cBus {
    type Error = VirtualI2cError;
}

impl embedded_hal::i2c::I2c<SevenBitAddress> for VirtualI2cBus {
    fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.hardware.transaction(self.bus, address, operations)
    }
}

impl embedded_hal_async::i2c::I2c<SevenBitAddress> for VirtualI2cBus {
    async fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.hardware.transaction(self.bus, address, operations)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal_async::i2c::I2c as _;

    use super::*;
    use crate::{clock::Clock, device::RegisterDevice};

    #[test]
    fn the_async_contract_reaches_the_same_device() {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");
        let mut read = [0; 1];
        embassy_futures::block_on(async {
            bus.write(0x50, &[0x01, 0x5a]).await.expect("write");
            bus.write_read(0x50, &[0x01], &mut read)
                .await
                .expect("read");
        });
        assert_eq!(read, [0x5a]);
    }

    #[test]
    fn errors_map_to_embedded_hal_kinds() {
        use embedded_hal::i2c::Error as _;
        assert_eq!(
            VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Address).kind(),
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        );
        assert_eq!(VirtualI2cError::Bus.kind(), ErrorKind::Bus);
        assert_eq!(VirtualI2cError::Timeout.kind(), ErrorKind::Other);
    }
}
