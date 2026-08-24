//! Concrete macOS Platform capabilities.

#![recursion_limit = "256"]

extern crate alloc;

mod flash;
mod layout;
mod network;
mod platform;
mod tun;

pub use flash::{FileNorFlash, FileNorFlashError, VolatileNorFlash, VolatileNorFlashError};
pub use layout::{FileLayout, FileLayoutError, FileRegion, FileRegionAccess};
pub use network::{MacosNetworkError, NETWORK_FD_ENV};
pub use platform::{
    MacosPartition, MacosPartitions, MacosPlatform, MacosPlatformError, MacosSettings,
};
pub use tun::{GATEWAY_ADDRESS, STACK_ADDRESS};

include!(concat!(env!("OUT_DIR"), "/macos_config.rs"));
