use core::{ffi::c_int, marker::PhantomData, mem::transmute};

use crate::{
    Thread,
    cdef::{EventMask, lua_Hook, lua_State},
};

use super::{ArInfo, ThreadDebug};

impl<const ID_SIZE: usize> ThreadDebug<'_, ID_SIZE> {
    /// Sets the debugging hook function.
    ///
    /// `mask` specifies on which events the hook will be called:
    /// it is formed by [`EventMask`].
    /// Hooks are disabled by supplying an empty `mask`.
    ///
    /// `count` is only meaningful when the mask includes [`EventMask::COUNT`].
    ///
    /// For each event, the hook is called as explained below:
    /// - The **call** hook is called when the interpreter calls a function.
    ///   The hook is called just after Lua enters the new function.
    /// - The **return** hook is called when the interpreter returns from a function.
    ///   The hook is called just before Lua leaves the function.
    /// - The **line** hook is called when the interpreter is about to start the execution of a new line of code,
    ///   or when it jumps back in the code (even to the same line).
    ///   This event only happens while Lua is executing a Lua function.
    /// - The **count** hook is called after the interpreter executes every `count` instructions.
    ///   This event only happens while Lua is executing a Lua function.
    pub fn set_hook_fn(&self, f: Option<HookFunc<ID_SIZE>>, mask: EventMask, count: HookCount) {
        self.set_c_hook_fn(f.map(to_hook), mask, count);
    }
}

pub type HookCount = c_int;

/// Function to be called by the Lua debugger in specific events.
///
/// This type is compatible with [`lua_Hook`];
/// however, this may not be a safe assumption to make for the reverse.
/// Use [`to_hook`] to convert from this type if needed.
pub type HookFunc<const ID_SIZE: usize> =
    extern "C-unwind" fn(DebugCtx<'_, ID_SIZE>, &mut ArInfo<ID_SIZE>);

/// Converts a [`HookFunc`] to [`lua_Hook`].
pub const fn to_hook<const ID_SIZE: usize>(f: HookFunc<ID_SIZE>) -> lua_Hook<ID_SIZE> {
    unsafe { transmute(f) }
}

/// Context passed to a [`HookFunc`].
///
/// # Layout
/// This type has the same layout and ABI as [`*mut lua_State`](lua_State).
#[repr(transparent)]
pub struct DebugCtx<'a, const ID_SIZE: usize> {
    l: *mut lua_State,
    _thread: PhantomData<&'a mut Thread>,
}
