//! Platform heap snapshots and fragmentation indicators.

/// Point-in-time metrics from a platform allocator.
///
/// `total_bytes`, `free_bytes`, and `largest_free_block_bytes` should describe
/// the same heap/capability class. Mixing values from different heaps makes the
/// derived metrics meaningless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapSnapshot {
    /// Total capacity of the observed heap.
    pub total_bytes: usize,
    /// Capacity currently available for allocation.
    pub free_bytes: usize,
    /// Largest single allocation the free space can currently satisfy.
    pub largest_free_block_bytes: usize,
    /// Historical low-water mark, when the allocator exposes one.
    pub minimum_free_bytes: Option<usize>,
}

impl HeapSnapshot {
    /// Construct a snapshot without a historical low-water mark.
    pub const fn new(
        total_bytes: usize,
        free_bytes: usize,
        largest_free_block_bytes: usize,
    ) -> Self {
        Self {
            total_bytes,
            free_bytes,
            largest_free_block_bytes,
            minimum_free_bytes: None,
        }
    }

    /// Attach the allocator's historical minimum-free value.
    #[must_use]
    pub const fn with_minimum_free_bytes(mut self, minimum_free_bytes: usize) -> Self {
        self.minimum_free_bytes = Some(minimum_free_bytes);
        self
    }

    /// Bytes currently unavailable for allocation.
    pub const fn used_bytes(self) -> usize {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    /// Free bytes outside the largest currently allocatable block.
    ///
    /// This is a useful external-fragmentation indicator. It is not a complete
    /// description of allocator metadata, size classes, or internal waste.
    pub const fn fragmented_free_bytes(self) -> usize {
        self.free_bytes
            .saturating_sub(self.largest_free_block_bytes)
    }

    /// Estimate external fragmentation as `1 - largest_free / total_free`.
    ///
    /// Returns `None` when the heap has no free bytes. Values are clamped to
    /// `[0.0, 1.0]` if a platform returns inconsistent counters.
    pub fn external_fragmentation_ratio(self) -> Option<f64> {
        if self.free_bytes == 0 {
            return None;
        }

        let largest = self.largest_free_block_bytes.min(self.free_bytes);
        Some(1.0 - largest as f64 / self.free_bytes as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_usage_and_fragmented_free_bytes() {
        let snapshot = HeapSnapshot::new(1_024, 400, 250).with_minimum_free_bytes(128);

        assert_eq!(snapshot.used_bytes(), 624);
        assert_eq!(snapshot.fragmented_free_bytes(), 150);
        assert_eq!(snapshot.minimum_free_bytes, Some(128));
        assert_eq!(snapshot.external_fragmentation_ratio(), Some(0.375));
    }

    #[test]
    fn empty_heap_has_no_fragmentation_ratio() {
        let snapshot = HeapSnapshot::new(1_024, 0, 0);

        assert_eq!(snapshot.external_fragmentation_ratio(), None);
    }

    #[test]
    fn inconsistent_largest_block_is_clamped() {
        let snapshot = HeapSnapshot::new(1_024, 400, 500);

        assert_eq!(snapshot.fragmented_free_bytes(), 0);
        assert_eq!(snapshot.external_fragmentation_ratio(), Some(0.0));
    }
}
