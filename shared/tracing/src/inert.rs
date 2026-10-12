//! Disabled spans and events for targets without atomic compare-and-swap.

use core::marker::PhantomData;

/// A span that is never enabled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span(());

impl Span {
    /// A disabled span.
    #[must_use]
    pub const fn none() -> Self {
        Self(())
    }

    /// The current span, which is always disabled here.
    #[must_use]
    pub const fn current() -> Self {
        Self(())
    }

    /// Enters the span until the guard drops; does nothing here.
    #[must_use]
    pub const fn enter(&self) -> Entered<'_> {
        Entered(PhantomData)
    }

    /// Runs `f` inside the span; here it only runs `f`.
    pub fn in_scope<T>(&self, f: impl FnOnce() -> T) -> T {
        f()
    }

    /// Always true: no span is ever enabled.
    #[must_use]
    pub const fn is_none(&self) -> bool {
        true
    }

    /// Always true: no span is ever enabled.
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        true
    }
}

/// Guard returned by [`Span::enter`].
#[derive(Debug)]
pub struct Entered<'a>(PhantomData<&'a Span>);

/// Attaches a span to a future; here the future is returned unchanged.
pub trait Instrument: Sized {
    /// Returns `self`.
    #[must_use]
    fn instrument(self, _span: Span) -> Self {
        self
    }

    /// Returns `self`.
    #[must_use]
    fn in_current_span(self) -> Self {
        self
    }
}

impl<T> Instrument for T {}

/// Event and span verbosity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

impl Level {
    /// The most verbose level.
    pub const TRACE: Self = Self(0);
    /// Debug output.
    pub const DEBUG: Self = Self(1);
    /// Informational output.
    pub const INFO: Self = Self(2);
    /// Warnings.
    pub const WARN: Self = Self(3);
    /// Errors.
    pub const ERROR: Self = Self(4);
}

/// Creates a disabled span.
#[macro_export]
macro_rules! span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Creates a disabled span.
#[macro_export]
macro_rules! trace_span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Creates a disabled span.
#[macro_export]
macro_rules! debug_span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Creates a disabled span.
#[macro_export]
macro_rules! info_span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Creates a disabled span.
#[macro_export]
macro_rules! warn_span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Creates a disabled span.
#[macro_export]
macro_rules! error_span {
    ($($arguments:tt)*) => {
        $crate::Span::none()
    };
}

/// Records nothing.
#[macro_export]
macro_rules! event {
    ($($arguments:tt)*) => {{}};
}

/// Records nothing.
#[macro_export]
macro_rules! trace {
    ($($arguments:tt)*) => {{}};
}

/// Records nothing.
#[macro_export]
macro_rules! debug {
    ($($arguments:tt)*) => {{}};
}

/// Records nothing.
#[macro_export]
macro_rules! info {
    ($($arguments:tt)*) => {{}};
}

/// Records nothing.
#[macro_export]
macro_rules! warn {
    ($($arguments:tt)*) => {{}};
}

/// Records nothing.
#[macro_export]
macro_rules! error {
    ($($arguments:tt)*) => {{}};
}
