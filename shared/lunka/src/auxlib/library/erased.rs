use crate::cdef::auxlib::luaL_Reg;

use super::{ArrayLibrary, Library};

/// Type-erased [`Library`] that lives for `'static`.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct StaticLibrary {
    static_regs: *const [luaL_Reg],
}
impl StaticLibrary {
    /// Type-erase an [`ArrayLibrary`].
    pub const fn from_array<const N: usize>(lib: &'static ArrayLibrary<'_, N>) -> Self {
        Self {
            static_regs: lib.as_terminated_slice(),
        }
    }
}
unsafe impl Library for StaticLibrary {
    fn length(&self) -> usize {
        self.static_regs.len()
    }
    fn as_ptr(&self) -> *const luaL_Reg {
        self.static_regs as *const _
    }
}
