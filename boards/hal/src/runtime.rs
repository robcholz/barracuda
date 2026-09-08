//! Protocol-neutral ownership for physical resources exposed at runtime.

extern crate alloc;

use alloc::string::String;
use core::fmt;

/// Physical resource category used for lookup and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceKind {
    /// One package pin or pad.
    Pin,
    /// One peripheral controller instance.
    Controller,
    /// One DMA resource.
    Dma,
    /// One timer instance.
    Timer,
    /// One controller-owned channel.
    Channel,
    /// A Platform-defined physical resource.
    Other,
}

/// Failure while reserving runtime physical resources.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseError {
    /// A requested Board name is absent or has the wrong resource category.
    NotExposed {
        /// Requested application-visible name.
        resource: String,
        /// Expected resource category.
        kind: ResourceKind,
    },
    /// One request assigns the same physical resource to more than one role.
    Duplicate {
        /// Repeated application-visible resource name.
        resource: String,
    },
    /// Another active function currently owns the physical resource.
    Busy {
        /// Application or Platform diagnostic resource name.
        resource: &'static str,
        /// Function class that currently owns the resource.
        owner: &'static str,
    },
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotExposed { resource, kind } => {
                write!(formatter, "{kind:?} resource `{resource}` is not exposed")
            }
            Self::Duplicate { resource } => {
                write!(
                    formatter,
                    "resource `{resource}` is used by more than one role"
                )
            }
            Self::Busy { resource, owner } => {
                write!(formatter, "resource `{resource}` is busy as `{owner}`")
            }
        }
    }
}

impl core::error::Error for LeaseError {}
