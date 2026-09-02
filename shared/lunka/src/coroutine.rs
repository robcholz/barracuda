use core::ops::{Deref, DerefMut};

use crate::{Thread, cdef::Status};

/// Data structure that represents a Lua coroutine.
///
/// See also [`Lua`](crate::Lua) for the main thread.
///
/// This type does not have a [`Drop`] implementation.
/// Any threads that are not used anymore must either be closed manually with
/// [`Coroutine::close`] or left to be garbage-collected by Lua.
#[derive(Debug)]
#[repr(transparent)]
pub struct Coroutine<'l> {
    thread: &'l mut Thread,
}

impl<'l> Coroutine<'l> {
    /// Wrap a Lua [`Thread`] as if it is a coroutine.
    pub const fn new(thread: &'l mut Thread) -> Self {
        Self { thread }
    }
}

impl AsRef<Thread> for Coroutine<'_> {
    fn as_ref(&self) -> &Thread {
        self.thread
    }
}

impl AsMut<Thread> for Coroutine<'_> {
    fn as_mut(&mut self) -> &mut Thread {
        self.thread
    }
}

impl Deref for Coroutine<'_> {
    type Target = Thread;
    fn deref(&self) -> &Self::Target {
        self.thread
    }
}

impl DerefMut for Coroutine<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.thread
    }
}

impl Coroutine<'_> {
    /// Alias to [`Thread::close_as_coroutine`].
    pub fn close(&mut self) -> Status {
        self.thread.close_as_coroutine()
    }

    /// Alias to [`Thread::close_as_coroutine_from`].
    pub fn close_from(&mut self, from: &Self) -> Status {
        self.thread.close_as_coroutine_from(from.thread)
    }
}
