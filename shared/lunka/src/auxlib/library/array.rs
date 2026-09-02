use core::{ffi::CStr, marker::PhantomData, slice::from_raw_parts};

use crate::{
    Func,
    cdef::{auxlib::luaL_Reg, lua_CFunction},
    to_c_function,
};

use super::Library;

/// Array-backed [`Library`].
///
/// The generic `const N: usize` specifies the static number of entries;
/// `'name` indicates the lifetime for every key, which are [`CStr`]s.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ArrayLibrary<'name, const N: usize> {
    regs: [luaL_Reg; N],
    terminator: luaL_Reg,
    _life: PhantomData<&'name CStr>,
}

impl ArrayLibrary<'_, 0> {
    /// Returns an empty list.
    pub const fn empty() -> Self {
        unsafe { Self::from_regs([]) }
    }
}

impl<'name, const N: usize> ArrayLibrary<'name, N> {
    /// Constructs a list from an array of [`luaL_Reg`].
    ///
    /// # Safety
    /// Every C string in `regs` must be valid for reads throughout the lifetime `'name`.
    pub const unsafe fn from_regs(regs: [luaL_Reg; N]) -> Self {
        Self {
            regs,
            terminator: luaL_Reg::NULL,
            _life: PhantomData,
        }
    }

    /// Constructs a list with a static list of functions.
    pub const fn from_c_functions(items: [(&'name CStr, Option<lua_CFunction>); N]) -> Self {
        let mut regs = [luaL_Reg::NULL; N];
        let mut i = 0;
        while i < N {
            regs[i] = luaL_Reg {
                name: items[i].0.as_ptr(),
                func: items[i].1,
            };
            i += 1;
        }
        unsafe { Self::from_regs(regs) }
    }

    /// Constructs a list with a static list of functions.
    pub const fn from_funcs(items: [(&'name CStr, Option<Func>); N]) -> Self {
        let mut regs = [luaL_Reg::NULL; N];
        let mut i = 0;
        while i < N {
            regs[i] = luaL_Reg {
                name: items[i].0.as_ptr(),
                func: match items[i].1 {
                    Some(f) => Some(to_c_function(f)),
                    None => None,
                },
            };
            i += 1;
        }
        unsafe { Self::from_regs(regs) }
    }

    /// Returns a pointer to this structure to be used with C.
    pub const fn as_ptr(&self) -> *const luaL_Reg {
        self.regs.as_ptr()
    }

    /// Returns a terminated slice of [`luaL_Reg`]s that represent the registered functions.
    pub const fn as_terminated_slice(&self) -> &[luaL_Reg] {
        unsafe { from_raw_parts(self.as_ptr(), N + 1) }
    }
}

unsafe impl<const N: usize> Library for ArrayLibrary<'_, N> {
    fn length(&self) -> usize {
        N
    }
    fn as_ptr(&self) -> *const luaL_Reg {
        self.as_ptr()
    }
}
