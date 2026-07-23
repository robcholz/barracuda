//! Profiling utilities shared by Claw workload harnesses.
//!
//! This crate contains measurement infrastructure, not the workloads being
//! measured. A workload executable owns its backend fixtures, installs a global
//! allocator when needed, and drives the target crate through its public API.
//!
//! DHAT reports allocation volume, peak live memory, retained memory, and
//! allocation lifetimes. It does not expose an allocator's free-list layout, so
//! actual external fragmentation must come from a platform heap probe and can be
//! represented with [`HeapSnapshot`].

mod heap;

pub use heap::HeapSnapshot;

#[cfg(all(feature = "dhat-heap", not(target_os = "espidf")))]
pub mod dhat;

/// Install DHAT as the executable's global allocator.
///
/// Invoke this once at crate scope in a profiling executable. The allocator is
/// intentionally not installed by the `claw-profile` library itself: global
/// allocator selection belongs to the final linked executable.
#[cfg(all(feature = "dhat-heap", not(target_os = "espidf")))]
#[macro_export]
macro_rules! install_dhat_allocator {
    () => {
        #[global_allocator]
        static CLAW_PROFILE_DHAT_ALLOCATOR: $crate::dhat::Allocator = $crate::dhat::Allocator;
    };
}
