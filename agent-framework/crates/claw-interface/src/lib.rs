#![no_std]

//! `claw_interface` — the OS / platform abstraction layer for the claw Rust
//! crates.
//!
//! This is the inbound boundary (C / OS -> Rust): it defines the
//! dependency-injection traits that abstract over filesystem facilities.
//! Networking is provided by `claw-net`; time is provided globally by
//! `embassy-time`.

extern crate alloc;
#[cfg(feature = "diskfs")]
extern crate std;

pub mod fs;

pub use fs::{ClawFile, ClawFs, FsError, FsIoError};
#[cfg(feature = "diskfs")]
pub use fs::{DiskFile, DiskFs};
pub use fs::{MemFile, MemFs};
