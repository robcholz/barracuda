//! Cooperative Component lifecycle and routing runtime.

#![no_std]

extern crate alloc;

mod component;
mod router;

pub use component::{
    Component, ComponentError, ComponentFuture, ComponentResult, RegisterContext, RunContext,
    UnregisterContext,
};
pub use router::{
    CleanupError, ComponentCleanupFailure, ComponentId, LoadError, Router, RouterError, UnloadError,
};
