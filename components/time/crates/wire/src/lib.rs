//! Fixed-layout Time RPC DTOs shared by the Component and schema build.
#![cfg_attr(not(feature = "schema"), no_std)]

use getset::CopyGetters;
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Empty request object accepted by `time.now`.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimeNowRequest {
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    reserved: u8,
}

impl TimeNowRequest {
    /// Creates an empty `time.now` request.
    #[must_use]
    pub const fn new() -> Self {
        Self { reserved: 0 }
    }
}

#[cfg(feature = "schema")]
barracuda_rpc_schema::register!(TimeNowRequest);

/// Current authoritative UTC calendar time.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct TimeNow {
    /// UTC year.
    #[getset(get_copy = "pub")]
    year: u16,
    /// UTC month in the range 1 through 12.
    #[getset(get_copy = "pub")]
    month: u8,
    /// UTC day of month.
    #[getset(get_copy = "pub")]
    day: u8,
    /// UTC hour in the range 0 through 23.
    #[getset(get_copy = "pub")]
    hour: u8,
    /// UTC minute in the range 0 through 59.
    #[getset(get_copy = "pub")]
    minute: u8,
    /// UTC second in the range 0 through 59.
    #[getset(get_copy = "pub")]
    second: u8,
    #[serde(skip)]
    reserved: u8,
}

impl TimeNow {
    /// Creates one validated-by-source UTC calendar response.
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            reserved: 0,
        }
    }
}

/// Business-level failure returned by `time.now`.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum TimeRpcError {
    /// No successful network synchronization has completed since boot.
    Unsynchronized = 0,
    /// The last network synchronization exceeded the configured holdover.
    Stale = 1,
    /// The synchronized value cannot be represented as a calendar time.
    OutOfRange = 2,
}
