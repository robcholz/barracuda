//! Virtual GPIO and I2C hardware for the host Platforms.
//!
//! The Linux and macOS Platforms expose this crate's [`hal`] module as their
//! own, so host Boards declare exposed virtual pins and controllers exactly
//! like device Boards. One process-wide [`VirtualHardware`] model sits behind
//! them; its manager answers a line-delimited JSON control protocol on the
//! loopback address in `BARRACUDA_VIRTUAL_IO_ADDR` (see `README.md`).
//!
//! Device models ([`I2cDevice`]) are independent of the manager and of the
//! System: a chip driver test can attach a model to a standalone
//! [`VirtualHardware`] with a manual [`Clock`], run the driver over
//! [`VirtualI2cBus`] and [`VirtualDelay`], and assert the register contents
//! and the datasheet order and timing violations the model reports.
//!
//! The crate is host-only; it never enters firmware.

mod clock;
pub mod control;
pub mod declared;
pub mod device;
mod gpio;
pub mod hal;
mod hardware;
mod i2c;
pub mod models;
mod spi;

pub use clock::{Clock, VirtualDelay};
pub use device::{DataNack, DeviceContext, I2cDevice, RegisterDevice, RuleViolation};
pub use gpio::VirtualDigitalPin;
pub use hardware::{
    hex, unhex, BusSnapshot, DeviceSnapshot, Event, EventDetail, FaultKind, FaultRule,
    HardwareError, OperationRecord, PinDrive, PinMode, PinPull, PinSnapshot, RecordedViolation,
    VirtualHardware, EVENT_CAPACITY,
};
pub use i2c::{VirtualI2cBus, VirtualI2cError};
pub use spi::{SpiDevice, VirtualSpiBus, VirtualSpiError};
