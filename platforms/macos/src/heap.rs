//! Host accounting for the ordinary heap, kept apart from bulk memory.
//!
//! Device Platforms split the ordinary heap (internal RAM) from the bulk-memory
//! domain (for example PSRAM). This host Platform mirrors that split so runs
//! approximate what a device must hold in internal RAM: the global allocator
//! counts ordinary allocations and can enforce a budget, while
//! `BulkVec`/`BulkBox` use the system allocator directly and are not counted.
//! The count also includes host-only allocations (the I/O reactor, logger,
//! and TUN driver), so it is an upper bound on the device figure.
//!
//! Set `BARRACUDA_HEAP_LIMIT_BYTES` to run with a device-sized ordinary heap;
//! an allocation beyond the budget aborts the process like a device OOM.

use std::alloc::System;

use barracuda_bulk_memory::platform::Backend;
use cap::Cap;
use embassy_time::{Duration, Timer};

#[global_allocator]
static HEAP: Cap<System> = Cap::new(System, usize::MAX);
static BULK_ALLOCATOR: allocator_api2::alloc::System = allocator_api2::alloc::System;
static BULK_BACKEND: Backend = Backend::new(&BULK_ALLOCATOR);

/// Environment variable that selects the ordinary-heap budget in bytes.
const HEAP_LIMIT_VARIABLE: &str = "BARRACUDA_HEAP_LIMIT_BYTES";
/// High-water growth between two reports.
const REPORT_STEP_BYTES: usize = 4 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// Selects the uncounted bulk domain and applies the optional heap budget.
pub(crate) fn install() {
    barracuda_bulk_memory::platform::install(&BULK_BACKEND);
    let Ok(value) = std::env::var(HEAP_LIMIT_VARIABLE) else {
        return;
    };
    match value.parse::<usize>() {
        Ok(limit) if HEAP.set_limit(limit).is_err() => {
            log::warn!("ordinary heap already exceeds the {limit}-byte budget");
        }
        Ok(limit) => log::info!("ordinary heap budget: {limit} bytes"),
        Err(_error) => log::warn!("ignoring invalid {HEAP_LIMIT_VARIABLE} `{value}`"),
    }
}

/// Logs the ordinary-heap high-water mark whenever it grows by a report step.
#[embassy_executor::task]
pub(crate) async fn report_high_water() {
    let mut reported = 0;
    loop {
        let peak = HEAP.max_allocated();
        if peak >= reported + REPORT_STEP_BYTES {
            log::info!(
                "ordinary heap high-water: {peak} bytes (current {})",
                HEAP.allocated()
            );
            reported = peak;
        }
        Timer::after(REPORT_INTERVAL).await;
    }
}
