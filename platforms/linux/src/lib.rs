//! Concrete Linux Platform capabilities.

#![recursion_limit = "256"]

extern crate alloc;

mod application;
/// Linux HAL: the host-only virtual GPIO and I2C hardware shared with the
/// other host Platform, controlled through its peripherals manager.
pub mod hal {
    pub use barracuda_platform_virtual_io::hal::*;
}
mod entropy;
mod flash;
mod heap;
mod layout;
mod logging;
mod network;
mod platform;
mod tun;

pub use entropy::LinuxEntropy;
pub use flash::{FileNorFlash, FileNorFlashError};
pub use layout::{FileLayout, FileLayoutError, FileRegion, FileRegionAccess};
pub use network::LinuxNetworkError;
pub use platform::{
    LinuxPartition, LinuxPartitions, LinuxPlatform, LinuxPlatformError, LinuxSettings,
};
pub use tun::{GATEWAY_ADDRESS, STACK_ADDRESS};

include!(concat!(env!("OUT_DIR"), "/linux_config.rs"));
