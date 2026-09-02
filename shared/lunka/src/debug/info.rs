use core::{
    ffi::{CStr, c_int, c_uchar, c_ushort},
    fmt,
    ops::{Deref, DerefMut},
    slice::from_raw_parts,
};

use crate::cdef::{DEFAULT_ID_SIZE, Event, lua_Debug};

/// Information about a function or activation record (function invocation).
///
/// # Layout
/// This type has the same layout and ABI as [`lua_Debug<ID_SIZE>`].
#[derive(PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct DebugInfo<const ID_SIZE: usize> {
    raw: lua_Debug<ID_SIZE>,
}

impl<const ID_SIZE: usize> DebugInfo<ID_SIZE> {
    /// Wraps a [`lua_Debug<ID_SIZE>`].
    ///
    /// # Safety
    /// `raw` must only have valid values.
    pub const unsafe fn from_raw(raw: lua_Debug<ID_SIZE>) -> Self {
        Self { raw }
    }

    /// Returns an immutable reference to the inner [`lua_Debug<ID_SIZE>`].
    pub const fn as_raw(&self) -> &lua_Debug<ID_SIZE> {
        &self.raw
    }

    /// Returns a mutable reference to the inner [`lua_Debug<ID_SIZE>`].
    ///
    /// # Safety
    /// The returned structure must not be mutated such that it contains invalid values.
    pub const unsafe fn as_mut_raw(&mut self) -> &mut lua_Debug<ID_SIZE> {
        &mut self.raw
    }

    /// Returns the event number.
    ///
    /// This is only relevant for debug hooks,
    /// and is otherwise meaningless.
    pub const fn event(&self) -> Event {
        Event(self.as_raw().event)
    }

    /// Returns the name of the callee.
    ///
    /// Requires name information.
    pub const fn name(&self) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(self.as_raw().name) }
    }

    /// Returns the name of what the callee is.
    ///
    /// Typically this is `global`, `local`, `field` or `method`.
    ///
    /// Requires name information.
    pub const fn name_what(&self) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(self.as_raw().namewhat) }
    }

    /// Returns what the callee is.
    ///
    /// Typically this is `Lua`, `C`, `main` or `tail`.
    ///
    /// Requires source information.
    pub const fn what(&self) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(self.as_raw().what) }
    }

    /// Returns the source of the callee.
    ///
    /// Typically this is `Lua`, `C`, `main` or `tail`.
    ///
    /// Requires source information.
    pub const fn source(&self) -> Option<&[u8]> {
        let raw = self.as_raw();
        let ptr = raw.source as *const u8;
        if !ptr.is_null() {
            unsafe { Some(from_raw_parts(ptr, raw.srclen)) }
        } else {
            None
        }
    }

    /// Returns the current line in the code.
    pub const fn current_line(&self) -> c_int {
        self.as_raw().currentline
    }

    /// Returns the first line of code where the callee is defined.
    ///
    /// Requires source information.
    pub const fn line_defined(&self) -> c_int {
        self.as_raw().linedefined
    }

    /// Returns the last line of code where the callee is defined.
    ///
    /// Requires source information.
    pub const fn last_line_defined(&self) -> c_int {
        self.as_raw().lastlinedefined
    }

    /// Returns the number of upvalues assigned to the callee.
    ///
    /// Requires function prototype information.
    pub const fn n_upvalues(&self) -> c_uchar {
        self.as_raw().nups
    }

    /// Returns the number of parameters for the callee.
    ///
    /// Requires function prototype information.
    pub const fn n_parameters(&self) -> c_uchar {
        self.as_raw().nparams
    }

    /// Returns `true` if the callee is variadic.
    ///
    /// Requires function prototype information.
    pub const fn is_variadic(&self) -> bool {
        self.as_raw().isvararg != 0
    }

    /// Returns `true` if the call is a *tail call*.
    ///
    /// Requires tall call information.
    pub const fn is_tail_call(&self) -> bool {
        self.as_raw().istailcall != 0
    }

    /// Returns the index of the first value transferred.
    ///
    /// Requires transfer information.
    pub const fn first_transferred(&self) -> c_ushort {
        self.as_raw().ftransfer
    }

    /// Returns the number of transferred values.
    ///
    /// Requires transfer information.
    pub const fn n_transferred(&self) -> c_ushort {
        self.as_raw().ntransfer
    }

    /// Returns a short version of the source string for the callee
    /// that can be displayed to a user.
    ///
    /// Requires source information.
    pub const fn short_src(&self) -> &CStr {
        let chars = &self.as_raw().short_src;
        let bytes = unsafe { from_raw_parts(chars.as_ptr() as *const u8, chars.len()) };
        match CStr::from_bytes_until_nul(bytes) {
            Ok(s) => s,
            Err(..) => c"",
        }
    }
}

impl<const ID_SIZE: usize> fmt::Debug for DebugInfo<ID_SIZE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct ByteString<'a>(pub &'a [u8]);
        impl fmt::Debug for ByteString<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("\"")?;
                fmt::Display::fmt(&self.0.escape_ascii(), f)?;
                f.write_str("\"")
            }
        }

        f.write_str("DebugInfo<")?;
        if ID_SIZE == DEFAULT_ID_SIZE {
            f.write_str("DEFAULT_ID_SIZE")
        } else {
            ID_SIZE.fmt(f)
        }?;
        f.write_str(">")?;
        f.debug_struct("")
            .field("event", &self.event())
            .field("name", &self.name())
            .field("name_what", &self.name_what())
            .field("what", &self.what())
            .field("source", &self.source().map(ByteString))
            .field("current_line", &self.current_line())
            .field("line_defined", &self.line_defined())
            .field("last_line_defined", &self.last_line_defined())
            .field("n_upvalues", &self.n_upvalues())
            .field("n_parameters", &self.n_parameters())
            .field("is_variadic", &self.is_variadic())
            .field("is_tail_call", &self.is_tail_call())
            .field("first_transferred", &self.first_transferred())
            .field("n_transferred", &self.n_transferred())
            .field("short_src", &self.short_src())
            .finish()
    }
}

/// Debugging information about a function.
///
/// # Layout
/// This type has the same layout and ABI as [`DebugInfo<ID_SIZE>`].
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct FuncInfo<const ID_SIZE: usize> {
    inner: DebugInfo<ID_SIZE>,
}

impl<const ID_SIZE: usize> FuncInfo<ID_SIZE> {
    /// Consumes a [`DebugInfo`],
    /// assuming that it contains information about a function.
    ///
    /// # Safety
    /// `inner` must contain information about a function.
    pub const unsafe fn from_inner(inner: DebugInfo<ID_SIZE>) -> Self {
        Self { inner }
    }
}

impl<const ID_SIZE: usize> Deref for FuncInfo<ID_SIZE> {
    type Target = DebugInfo<ID_SIZE>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl<const ID_SIZE: usize> DerefMut for FuncInfo<ID_SIZE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Debugging information about an activation record.
///
/// # Layout
/// This type has the same layout and ABI as [`DebugInfo<ID_SIZE>`].
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct ArInfo<const ID_SIZE: usize> {
    inner: DebugInfo<ID_SIZE>,
}

impl<const ID_SIZE: usize> ArInfo<ID_SIZE> {
    /// Consumes an [`ArInfo`],
    /// assuming that it contains information about an activation record.
    ///
    /// # Safety
    /// `inner` must contain information about an activation record.
    pub const unsafe fn from_inner(inner: DebugInfo<ID_SIZE>) -> Self {
        Self { inner }
    }
}

impl<const ID_SIZE: usize> Deref for ArInfo<ID_SIZE> {
    type Target = DebugInfo<ID_SIZE>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl<const ID_SIZE: usize> DerefMut for ArInfo<ID_SIZE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
