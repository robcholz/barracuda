use core::ffi::c_int;

/// Lua garbage collection mode setup information.
///
/// This structure is used for [`Thread::switch_gc_to`](crate::Thread::switch_gc_to).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GcMode {
    /// Incremental mode.
    Incremental {
        /// How long the collector should wait before starting a new cycle.
        /// Default is `200`, maximum is `1000`
        ///
        /// The collector starts a new cycle when the use of memory hits `pause`%
        /// of the use after the previous collection.
        /// Larger values make the collector less aggressive.
        ///
        /// Values equal to or less than `100` mean the collector will not wait
        /// to start a new cycle.
        /// A value of `200` means that the collector waits for the total memory
        /// in use to double before starting a new cycle.
        pause: u16,
        /// Speed of the collector relative to memory allocation; that is,
        /// how many elements it marks or sweeps for each kilobyte of memory
        /// allocated.
        /// Default is `100`, maximum is `1000`.
        ///
        /// Larger values make the collector more aggressive but also increase
        /// the size of each incremental step.
        ///
        /// You should not use values less than `100`, because they make the
        /// collector too slow and can result in the collector never finishing a
        /// cycle.
        step_multiplier: u16,
        /// Size of each incremental step, specifically how many bytes the
        /// interpreter allocates before performing a step.
        /// Default is `13` (steps of approximately `8` kilobytes).
        ///
        /// This parameter is logarithmic: A value of `step_size` means the
        /// interpreter will allocate `2 ^ step_size` bytes between steps and
        /// perform equivalent work during the step.
        ///
        /// A large value (e.g., `60`) makes the collector a stop-the-world
        /// (non-incremental) collector.
        step_size: c_int,
    },
    /// Generational mode.
    Generational {
        /// Frequency of minor collections.
        /// Default is `20`, maximum is `200`.
        ///
        /// For a minor multiplier `minor_mul`, a new minor collection will be
        /// done when memory grows `minor_mul`% larger than the memory in use
        /// after the previous major collection.
        ///
        /// For instance, for a multiplier of `20`, the collector will do a
        /// minor collection when the use of memory gets `20%` larger than the
        /// use after the previous major collection.
        minor_mul: u8,
        /// Frequency of major collections.
        /// Default is `100`, maximum is `1000`.
        ///
        /// For a major multiplier `major_mul`, a new major collection will be
        /// done when memory grows `major_mul`% larger than the memory in use
        /// after the previous major collection.
        ///
        /// For instance, for a multiplier of `100`, the collector will do a
        /// major collection when the use of memory gets larger than twice the
        /// use after the previous collection.
        major_mul: u16,
    },
}
