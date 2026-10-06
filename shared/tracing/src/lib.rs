//! `tracing` for code that must also build on targets without atomic
//! compare-and-swap.
//!
//! Where the target has pointer-width atomic read-modify-write, this crate
//! re-exports `tracing` unchanged. Elsewhere, such as on ESP32-C3 and
//! ESP32-S2, it provides the same names as inert stand-ins: spans are always
//! disabled and events compile to nothing. Depend on it under the name
//! `tracing` so call sites stay `tracing::info_span!(..)`.

#![no_std]

#[cfg(target_has_atomic = "ptr")]
pub use tracing::*;

#[cfg(not(target_has_atomic = "ptr"))]
mod inert;
#[cfg(not(target_has_atomic = "ptr"))]
pub use inert::*;
