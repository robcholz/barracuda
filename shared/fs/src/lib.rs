#![no_std]

//! `barracuda_fs` — the OS / platform abstraction layer for the barracuda Rust
//! crates.
//!
//! This is the inbound boundary (C / OS -> Rust): it defines the
//! dependency-injection traits that abstract over filesystem facilities.
//! Networking is provided by `barracuda-net`; time is provided globally by
//! `embassy-time`.

extern crate alloc;
#[cfg(feature = "diskfs")]
extern crate std;

pub mod fs;

#[cfg(feature = "diskfs")]
pub use fs::{DiskFile, DiskFs};
pub use fs::{FileSystem, FsError, FsFile, FsIoError};
pub use fs::{MemFile, MemFs};
