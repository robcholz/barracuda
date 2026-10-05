//! Concrete Linux Platform capabilities.

#![recursion_limit = "256"]

extern crate alloc;

mod application;
mod flash;
mod heap;
mod layout;
mod logging;
mod network;
mod platform;
mod tls;
mod tun;

pub use flash::{FileNorFlash, FileNorFlashError};
pub use layout::{FileLayout, FileLayoutError, FileRegion, FileRegionAccess};
pub use network::LinuxNetworkError;
pub use platform::{
    LinuxPartition, LinuxPartitions, LinuxPlatform, LinuxPlatformError, LinuxSettings,
};
pub use tls::LinuxTlsError;
pub use tun::{GATEWAY_ADDRESS, STACK_ADDRESS};

include!(concat!(env!("OUT_DIR"), "/linux_config.rs"));
