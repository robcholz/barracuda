//! Host heap profiling backed by the `dhat` crate.

use std::path::Path;

pub use ::dhat::Alloc as Allocator;

/// Allocation counters captured by DHAT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocationStats {
    /// Number of allocations made during the profile.
    pub total_allocations: u64,
    /// Sum of the sizes of all allocations made during the profile.
    pub total_bytes: u64,
    /// Number of profiled allocations still live at the snapshot.
    pub current_allocations: usize,
    /// Bytes in profiled allocations still live at the snapshot.
    pub current_bytes: usize,
    /// Number of live allocations when live bytes reached their global peak.
    pub peak_allocations: usize,
    /// Maximum live bytes observed during the profile.
    pub peak_bytes: usize,
}

impl From<::dhat::HeapStats> for AllocationStats {
    fn from(stats: ::dhat::HeapStats) -> Self {
        Self {
            total_allocations: stats.total_blocks,
            total_bytes: stats.total_bytes,
            current_allocations: stats.curr_blocks,
            current_bytes: stats.curr_bytes,
            peak_allocations: stats.max_blocks,
            peak_bytes: stats.max_bytes,
        }
    }
}

/// RAII scope for one DHAT heap profile.
///
/// Only one scope may run in a process at a time. Prefer one workload scenario
/// per process so startup, retained allocations, and output remain unambiguous.
#[must_use = "dropping the profile ends measurement and writes its output"]
#[derive(Debug)]
pub struct HeapProfile {
    _inner: ::dhat::Profiler,
}

impl HeapProfile {
    /// Start profiling allocations into `output_file`.
    ///
    /// The caller must install [`Allocator`] as the executable's global
    /// allocator, normally through [`crate::install_dhat_allocator!`].
    ///
    /// # Panics
    ///
    /// Panics if another DHAT profiler is already active in this process.
    pub fn start(output_file: impl AsRef<Path>) -> Self {
        Self {
            _inner: ::dhat::Profiler::builder().file_name(output_file).build(),
        }
    }

    /// Capture the counters at this point without ending the profile.
    pub fn stats(&self) -> AllocationStats {
        ::dhat::HeapStats::get().into()
    }

    /// Capture final counters, end profiling, and write the DHAT output.
    pub fn finish(self) -> AllocationStats {
        let stats = self.stats();
        drop(self);
        stats
    }
}
