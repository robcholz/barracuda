use core::sync::atomic::{AtomicBool, Ordering};

/// Cooperative cancellation token backed by a caller-owned atomic flag.
#[derive(Debug, Clone, Copy)]
pub struct Cancel<'a>(&'a AtomicBool);

impl<'a> Cancel<'a> {
    #[must_use]
    pub const fn new(flag: &'a AtomicBool) -> Self {
        Self(flag)
    }

    #[must_use]
    pub fn is_cancelled(self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl Cancel<'static> {
    #[must_use]
    pub fn never() -> Self {
        static NEVER: AtomicBool = AtomicBool::new(false);
        Self(&NEVER)
    }
}
