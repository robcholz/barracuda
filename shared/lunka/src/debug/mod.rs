use core::{
    ffi::{CStr, c_int},
    mem::transmute,
    ptr::null,
};

use crate::{Thread, cdef::*};

mod info;
pub use info::*;
mod flags;
pub use flags::*;
mod func;
pub use func::*;

impl Thread {
    /// Returns a [`ThreadDebug`] structure
    /// given the `ID_SIZE` for `self`.
    ///
    /// # Safety
    /// [`DEFAULT_ID_SIZE`] must be the appropriate identifier size for the underlying Lua state.
    pub const unsafe fn debug(&self) -> ThreadDebug<'_, DEFAULT_ID_SIZE> {
        unsafe { self.debug_with() }
    }

    /// Returns a [`ThreadDebug`] structure
    /// given the `ID_SIZE` for `self`.
    ///
    /// # Safety
    /// `ID_SIZE` must be the appropriate identifier size for the underlying Lua state.
    /// See [`DEFAULT_ID_SIZE`] for the default.
    pub const unsafe fn debug_with<const ID_SIZE: usize>(&self) -> ThreadDebug<'_, ID_SIZE> {
        ThreadDebug { thread: self }
    }
}

/// Wrapper for [`Thread`] that allows access to debug information.
#[repr(transparent)]
pub struct ThreadDebug<'a, const ID_SIZE: usize> {
    thread: &'a Thread,
}

impl<const ID_SIZE: usize> ThreadDebug<'_, ID_SIZE> {
    /// Gets information about a function invocation.
    ///
    /// Level `0` is the current running function,
    /// whereas level `n + 1` is the function that has called level `n`
    /// (except for tail calls, which do not count in the stack).
    /// When called with a level greater than the stack depth,
    /// this function returns `None`.
    pub fn get_ar_info(&self, what: DebugFlags, level: c_int) -> Option<ArInfo<ID_SIZE>> {
        let mut ar = self.get_stack(level)?;
        let ok = what.with_string(false, |what| unsafe {
            self.get_info_raw(what, ar.as_mut_raw())
        });
        ok.then_some(ar)
    }

    fn get_stack(&self, level: c_int) -> Option<ArInfo<ID_SIZE>> {
        let mut raw = raw_uninit();
        let ok = self.get_stack_raw(level, &mut raw);
        ok.then(move || unsafe { ArInfo::from_inner(DebugInfo::from_raw(raw)) })
    }

    /// Gets information about the function on the top of the stack.
    pub fn get_func_info(&self, what: DebugFlags) -> Option<FuncInfo<ID_SIZE>> {
        let mut raw = raw_uninit();
        let ok = what.with_string(true, |what| unsafe { self.get_info_raw(what, &mut raw) });
        ok.then(move || unsafe { FuncInfo::from_inner(DebugInfo::from_raw(raw)) })
    }

    /// Gets information about a local variable or a temporary value of
    /// a given activation record
    /// or function on the top of the stack.
    ///
    /// The index `n` selects which local variable to inspect.
    /// The function pushes the variable's value onto the stack
    /// and returns its name.
    /// It returns `None` (and pushes nothing) when the index is
    /// greater than the number of active local variables.
    ///
    /// For functions, only parameters of Lua functions are visible
    /// (as there is no information about what variables are active)
    /// and no values are pushed onto the stack.
    pub fn get_local<'dbg>(
        &self,
        ar: Option<&'dbg ArInfo<ID_SIZE>>,
        n: c_int,
    ) -> Option<&'dbg CStr> {
        unsafe { self.get_local_raw(ar.map(move |i| i.as_raw()), n) }
    }

    /// Sets the value of a local variable of a given activation record
    /// and returns its name.
    ///
    /// Returns `None` (and pops nothing) when the index is greater than the
    /// number of active local variables.
    ///
    /// This function assigns the value on the top of the stack to the variable.
    /// It also pops the value from the stack.
    pub fn set_local<'dbg>(&self, ar: &'dbg ArInfo<ID_SIZE>, n: c_int) -> Option<&'dbg CStr> {
        unsafe { self.set_local_raw(ar.as_raw(), n) }
    }
}

impl<const ID_SIZE: usize> ThreadDebug<'_, ID_SIZE> {
    const unsafe fn generic_hook_to_c(f: lua_Hook<ID_SIZE>) -> lua_Hook {
        unsafe { transmute(f) }
    }
    const unsafe fn c_hook_to_generic(f: lua_Hook) -> lua_Hook<ID_SIZE> {
        unsafe { transmute(f) }
    }

    /// Returns the current debugging hook function.
    ///
    /// See also [`lua_Hook`].
    pub fn c_hook_fn(&self) -> Option<lua_Hook<ID_SIZE>> {
        unsafe {
            lua_gethook(self.thread.as_ptr_inspect()).map(move |f| Self::c_hook_to_generic(f))
        }
    }

    /// Sets the debugging hook function.
    ///
    /// `hook` is the hook function.
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
    pub fn set_c_hook_fn(&self, hook_fn: Option<lua_Hook<ID_SIZE>>, mask: EventMask, count: c_int) {
        let hook_fn = unsafe { hook_fn.map(move |f| Self::generic_hook_to_c(f)) };
        unsafe { lua_sethook(self.thread.as_ptr_no_gc(), hook_fn, mask.0, count) }
    }

    /// Gets information about a specific function or function invocation,
    /// placing it into `ar`.
    ///
    /// # Safety
    /// For activation records, the parameter `ar` must be
    /// a valid activation record that was filled by
    /// a previous call to [`ThreadDebug::get_stack_raw`]
    /// or given as argument to a hook (see [`lua_Hook`]).
    ///
    /// For functions, `what` must contain `>`,
    /// and the function to be inspected must be on the top of the stack.
    pub unsafe fn get_info_raw(&self, what: &CStr, ar: &mut lua_Debug<ID_SIZE>) -> bool {
        (unsafe {
            lua_getinfo(
                self.thread.as_ptr_inspect(),
                what.as_ptr(),
                ar as *mut _ as *mut _,
            )
        }) != 0
    }

    /// Gets information about a local variable or a temporary value of
    /// a given activation record or function.
    ///
    /// The index `n` selects which local variable to inspect.
    /// The function pushes the variable's value onto the stack
    /// and returns its name.
    /// It returns `None` (and pushes nothing) when the index is
    /// greater than the number of active local variables.
    ///
    /// For functions, only parameters of Lua functions are visible
    /// (as there is no information about what variables are active)
    /// and no values are pushed onto the stack.
    ///
    /// # Safety
    /// For activation records, the parameter `ar` must be
    /// a valid activation record that was filled by
    /// a previous call to [`ThreadDebug::get_stack_raw`]
    /// or given as argument to a hook (see [`lua_Hook`]).
    ///
    /// For functions, `ar` must be `None`
    /// and the function to be inspected must be on the top of the stack.
    pub unsafe fn get_local_raw<'dbg>(
        &self,
        ar: Option<&'dbg lua_Debug<ID_SIZE>>,
        n: c_int,
    ) -> Option<&'dbg CStr> {
        let str_ptr = unsafe {
            lua_getlocal(
                self.thread.as_ptr_no_gc(),
                ar.map(|ar| ar as *const _ as *const _).unwrap_or(null()),
                n,
            )
        };
        unsafe { crate::util::opt_c_str(str_ptr) }
    }

    /// Gets information about the interpreter runtime stack.
    ///
    /// This function fills parts of a [`lua_Debug`] structure with an
    /// identification of the activation record of
    /// the function executing at a given level.
    ///
    /// Level `0` is the current running function,
    /// whereas level `n + 1` is the function that has called level `n`
    /// (except for tail calls, which do not count in the stack).
    /// When called with a level greater than the stack depth,
    /// this function returns `None`.
    pub fn get_stack_raw(&self, level: c_int, ar: &mut lua_Debug<ID_SIZE>) -> bool {
        unsafe { lua_getstack(self.thread.as_ptr_inspect(), level, ar as *mut _ as *mut _) != 0 }
    }

    /// Sets the value of a local variable of a given activation record
    /// and returns its name.
    ///
    /// Returns `None` (and pops nothing) when the index is greater than the
    /// number of active local variables.
    ///
    /// This function assigns the value on the top of the stack to the variable.
    /// It also pops the value from the stack.
    ///
    /// # Safety
    /// The parameter `ar` must be
    /// a valid activation record that was filled by
    /// a previous call to [`ThreadDebug::get_stack_raw`]
    /// or given as argument to a hook (see [`lua_Hook`]).
    pub unsafe fn set_local_raw<'dbg>(
        &self,
        ar: &'dbg lua_Debug<ID_SIZE>,
        n: c_int,
    ) -> Option<&'dbg CStr> {
        let str_ptr =
            unsafe { lua_setlocal(self.thread.as_ptr_no_gc(), ar as *const _ as *const _, n) };
        unsafe { crate::util::opt_c_str(str_ptr) }
    }
}

const fn raw_uninit<const ID_SIZE: usize>() -> lua_Debug<ID_SIZE> {
    lua_Debug {
        event: 0,
        name: null(),
        namewhat: null(),
        what: null(),
        source: null(),
        srclen: 0,
        currentline: -1,
        linedefined: -1,
        lastlinedefined: -1,
        nups: 0,
        nparams: 0,
        isvararg: 0,
        istailcall: 0,
        ftransfer: 0,
        ntransfer: 0,
        short_src: [0; ID_SIZE],
        i_ci: null(),
    }
}
