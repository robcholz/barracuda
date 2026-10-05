//! Concrete macOS Platform capabilities.

#![recursion_limit = "256"]

extern crate alloc;

mod application;
mod flash;
mod layout;
mod logging;
mod network;
mod platform;
mod tls;

pub use flash::{FileNorFlash, FileNorFlashError};
pub use layout::{FileLayout, FileLayoutError, FileRegion, FileRegionAccess};
pub use network::{MacosNetworkError, DNS_ADDRESS, GATEWAY_ADDRESS, STACK_ADDRESS};
pub use platform::{
    MacosPartition, MacosPartitions, MacosPlatform, MacosPlatformError, MacosSettings,
};
pub use tls::MacosTlsError;

include!(concat!(env!("OUT_DIR"), "/macos_config.rs"));
