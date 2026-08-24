//! Concrete host Platform capabilities.

#![recursion_limit = "256"]

extern crate alloc;

mod filesystem;
mod flash;
mod layout;
mod network;
mod platform;
mod webserver;

pub use filesystem::{DiskFile, DiskFs};
pub use flash::{FileNorFlash, FileNorFlashError, VolatileNorFlash, VolatileNorFlashError};
pub use layout::{HostLayout, HostLayoutError, HostRegion, HostRegionAccess};
pub use network::{TokioConnectedUdp, TokioConnection, TokioStack, TokioUnconnectedUdp};
pub use platform::{HostDatabaseRegion, HostPlatform, HostPlatformError, HostSettings};

include!(concat!(env!("OUT_DIR"), "/host_config.rs"));
