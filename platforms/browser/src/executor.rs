//! Embassy executor driven by the browser Worker event loop.

use core::cell::RefCell;

use embassy_executor::{raw, Spawner};

thread_local! {
    static EXECUTOR: RefCell<Option<&'static raw::Executor>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub fn start(initialize: impl FnOnce(Spawner)) -> Result<(), StartError> {
    let executor = Box::leak(Box::new(raw::Executor::new(core::ptr::null_mut())));
    EXECUTOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(StartError::AlreadyStarted);
        }
        *slot = Some(executor);
        Ok(())
    })?;
    initialize(executor.spawner());
    crate::ffi::pend_executor();
    Ok(())
}

#[unsafe(export_name = "__pender")]
fn pender(_context: *mut ()) {
    crate::ffi::pend_executor();
}

#[unsafe(no_mangle)]
pub extern "C" fn barracuda_browser_poll() {
    EXECUTOR.with(|slot| {
        if let Some(executor) = *slot.borrow() {
            // The JS host queues this call as a microtask, so it cannot poll
            // recursively from `__pender` or from another executor poll.
            unsafe { executor.poll() };
        }
    });
}

#[derive(Debug, thiserror::Error)]
#[doc(hidden)]
pub enum StartError {
    #[error("Browser executor is already running")]
    AlreadyStarted,
}
