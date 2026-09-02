use core::ffi::c_int;

use crate::{
    Managed,
    cdef::{auxlib::*, *},
};

mod array;
pub use array::*;
mod erased;
pub use erased::*;

mod macros;

/// Trait for collections of [`luaL_Reg`]
/// that are used in [`Managed::new_lib`].
///
/// # Safety
/// Every function must behave as documented.
/// See the documentation for more information.
pub unsafe trait Library {
    /// Returns the number of entries excluding the terminator.
    ///
    /// This number serves as a hint to Lua
    /// when allocating a table for the registered functions.
    fn length(&self) -> usize;
    /// Returns a pointer to an array of [`luaL_Reg`] terminated with [`luaL_Reg::NULL`].
    ///
    /// The array is valid for reads for the lifetime of `self`,
    /// and its length is indicated by the value returned by
    /// [`Library::length`] of the implementing type.
    fn as_ptr(&self) -> *const luaL_Reg;
}

impl Managed<'_> {
    /// Creates a new table and registers there the functions in the given [`Library`].
    ///
    /// _Unlike_ this function's C counterpart, this will _not_ call
    /// [`Thread::check_version`](crate::Thread::check_version).
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn new_lib<L: ?Sized + Library>(&mut self, library: &L) {
        unsafe {
            let l = self.as_ptr();
            lua_createtable(l, 0, library.length() as _);
            luaL_setfuncs(l, library.as_ptr(), 0);
        }
    }

    /// Creates a new table with a size optimized to
    /// store all entries in the given [`Library`]
    /// but does not actually store them.
    ///
    /// This function is intended to be used in conjunction with
    /// [`Managed::set_funcs`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn new_lib_table<L: ?Sized + Library>(&mut self, library: &L) {
        let _ = library;
        unsafe { lua_createtable(self.as_ptr(), 0, len_to_regs_len(library.length())) }
    }

    /// Registers all functions in the given [`Library`]
    /// into the table on the top of the stack
    /// (below optional upvalues).
    ///
    /// When `n_upvalues` is not zero, all functions are created with
    /// `n_upvalues` upvalues, initialized with copies of the values previously
    /// pushed on the stack on top of the library table.
    /// These values are popped from the stack after the registration.
    ///
    /// A value with a `None` value represents a placeholder, which is filled
    /// with `false`.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn set_funcs<L: ?Sized + Library>(&mut self, library: &L, n_upvalues: u8) {
        unsafe { luaL_setfuncs(self.as_ptr(), library.as_ptr(), n_upvalues as _) }
    }
}

const fn len_to_regs_len(len: usize) -> c_int {
    if len <= c_int::MAX as usize {
        len as _
    } else {
        c_int::MAX
    }
}
