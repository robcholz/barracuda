use std::io;

use super::{ClawThread, CoreAffinity, Priority, WorkerHandle};

/// Host implementation of [`ClawThread`] over `std::thread`. Zero-sized.
///
/// The embedded stack sizes (8-16 KiB) would overflow std's deeper frames, so
/// the requested `stack_size` is ignored and the platform default (multi-MiB)
/// stack is used; `priority` / `affinity` have no host analogue.
#[derive(Clone, Copy, Default)]
pub struct StdThread;

impl ClawThread for StdThread {
    fn spawn_worker<F>(
        name: &str,
        stack_size: usize,
        priority: Priority,
        affinity: CoreAffinity,
        f: F,
    ) -> io::Result<WorkerHandle>
    where
        F: FnOnce() + Send + 'static,
    {
        let _ = (stack_size, priority, affinity);
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(f)
            .map(WorkerHandle::new)
    }
}
