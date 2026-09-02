use core::{
    ffi::c_void,
    ops::{Deref, DerefMut},
};

use crate::{Thread, cdef::*};

#[cfg(feature = "auxlib")]
use crate::cdef::auxlib::*;

/// Data structure that represents a main Lua thread.
///
/// Unlike [`Coroutine`](crate::Coroutine),
/// this data structure has a [`Drop`] implementation that automatically closes (frees) the Lua state.
///
/// # Thread safety
/// [`Lua`] isn't [`Send`] nor [`Sync`] because of [`Thread`], which doesn't
/// implement any of those traits either.
/// Though, this structure *owns* its Lua thread, so, at first glance, it should
/// implement [`Send`], but implementing this marker trait is not valid.
///
/// Due to the unique nature of most [`Thread`] methods which take a `&self`
/// instead of a `&mut self`, a `Lua` *could've* been put into an `Arc<Lua>`,
/// and then internally mutated through the `&self` methods if it implemented
/// [`Send`].
///
/// # Examples
/// The Lua headers define there to be the macros `lua_lock` and `lua_unlock`.
/// These macros are intended to be used during compilation to synchronize
/// operations on a Lua state, possibly with a mutex.
/// The default definitions for these macros, however, are no-ops and will not
/// ensure any thread safety by default.
/// If you are *sure* that the Lua API you're linking against has been compiled
/// with meaningful `lua_lock` and `lua_unlock`, then you can wrap a `Lua` in
/// another type and implement [`Send`] and [`Sync`] for it, as well as
/// [`Deref`] and [`DerefMut`] for ergonomics:
/// ```no_run
/// use core::ops::{ Deref, DerefMut };
/// use lunka::Lua;
///
/// #[repr(transparent)]
/// pub struct MtLua {
/// 	lua: Lua,
/// }
///
/// impl Deref for MtLua {
/// 	type Target = Lua;
/// 	fn deref(&self) -> &Self::Target {
/// 		&self.lua
/// 	}
/// }
///
/// impl DerefMut for MtLua {
/// 	fn deref_mut(&mut self) -> &mut Self::Target {
/// 		&mut self.lua
/// 	}
/// }
///
/// // SAFETY: The Lua API we're linking against was compiled with
/// // synchronization for mutable operations.
/// unsafe impl Send for MtLua {}
/// unsafe impl Sync for MtLua {}
///
/// impl MtLua {
/// 	pub fn new() -> Self {
/// 		let lua = Lua::new();
/// 		Self {
/// 			lua
/// 		}
/// 	}
/// }
///
/// let lua = MtLua::new();
/// assert_eq!(lua.version(), lunka::cdef::VERSION_NUM);
/// ```
#[derive(Debug)]
#[repr(transparent)]
pub struct Lua {
    thread: &'static mut Thread,
}

impl Drop for Lua {
    fn drop(&mut self) {
        unsafe { self.thread.close_as_main() }
    }
}

impl AsRef<Thread> for Lua {
    fn as_ref(&self) -> &Thread {
        self.thread
    }
}

impl AsMut<Thread> for Lua {
    fn as_mut(&mut self) -> &mut Thread {
        self.thread
    }
}

impl Deref for Lua {
    type Target = Thread;

    fn deref(&self) -> &Self::Target {
        self.thread
    }
}

impl DerefMut for Lua {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.thread
    }
}

impl Lua {
    /// Construct a new [`Lua`] using an already-allocated Lua state.
    ///
    /// # Safety
    /// `l` must be a valid pointer to a Lua state.
    ///
    /// With this function, the [`Lua`] takes ownership of the Lua state.
    /// You may not, for example, pass a coroutine pointer to this, as the
    /// coroutine will not be owned by Rust code.
    pub unsafe fn from_ptr(l: *mut lua_State) -> Self {
        let thread = unsafe { Thread::from_ptr_mut(l) };
        Self { thread }
    }

    unsafe fn from_new_ptr(l: *mut lua_State) -> Option<Self> {
        if !l.is_null() {
            let lua = Self {
                thread: unsafe { Thread::from_ptr_mut(l) },
            };
            Some(lua)
        } else {
            None
        }
    }

    /// Construct a new [`Lua`] using the `lauxlib` function [`luaL_newstate`].
    ///
    /// Unlike [`Lua::try_new_auxlib`], this function never fails.
    #[cfg(feature = "auxlib")]
    pub fn new_auxlib() -> Self {
        match unsafe { Self::from_new_ptr(luaL_newstate()) } {
            Some(lua) => lua,
            _ => panic!("not enough memory to create Lua state using the `lauxlib.h` allocator"),
        }
    }

    /// Construct a new [`Lua`] using the `lauxlib` function [`luaL_newstate`].
    ///
    /// The function will return `None` if allocation failed.
    #[cfg(feature = "auxlib")]
    pub fn try_new_auxlib() -> Option<Self> {
        unsafe { Self::from_new_ptr(luaL_newstate()) }
    }

    /// Construct a new [`Lua`] using an allocation function (see [`lua_Alloc`]).
    ///
    /// Unlike [`Lua::try_new_with_alloc_fn`], this function never fails.
    ///
    /// # Safety
    /// `alloc_fn_data` must be valid to be passed to `alloc_fn`.
    pub unsafe fn new_with_alloc_fn(alloc_fn: lua_Alloc, alloc_fn_data: *mut c_void) -> Self {
        match unsafe { Self::from_new_ptr(lua_newstate(alloc_fn, alloc_fn_data)) } {
            Some(lua) => lua,
            _ => panic!("not enough memory to create Lua state using a custom allocator function"),
        }
    }

    /// Construct a new [`Lua`] using an allocation function (see [`lua_Alloc`]).
    ///
    /// The function will return `None` if allocation failed.
    ///
    /// # Safety
    /// `alloc_fn_data` must be valid to be passed to `alloc_fn`.
    pub unsafe fn try_new_with_alloc_fn(
        alloc_fn: lua_Alloc,
        alloc_fn_data: *mut c_void,
    ) -> Option<Self> {
        unsafe { Self::from_new_ptr(lua_newstate(alloc_fn, alloc_fn_data)) }
    }

    /// Construct a new [`Lua`] using the global Rust allocator,
    /// aligning all allocations to the alignment of [`MaxAlign`].
    ///
    /// # Panics
    /// This function will panic if allocating the Lua state failed.
    #[cfg(feature = "alloc")]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        match Self::try_new() {
            Some(lua) => lua,
            _ => panic!("not enough memory to create Lua state using the global Rust allocator"),
        }
    }

    /// Construct a new [`Lua`] using the global Rust allocator,
    /// aligning all allocations to the alignment of [`MaxAlign`].
    ///
    /// The function will return `None` if allocation failed.
    #[cfg(feature = "alloc")]
    pub fn try_new() -> Option<Self> {
        use alloc::alloc::{Layout, alloc, dealloc, realloc};
        use core::ptr::null_mut;

        fn guess_layout(size: usize) -> Option<Layout> {
            Layout::from_size_align(size, align_of::<MaxAlign>()).ok()
        }

        // This should be OK for emulating the typical allocation routine used with Lua.
        unsafe extern "C-unwind" fn l_alloc(
            _ud: *mut c_void,
            ptr: *mut c_void,
            osize: usize,
            nsize: usize,
        ) -> *mut c_void {
            let ptr = ptr as *mut u8;
            if !ptr.is_null() {
                if nsize == 0 {
                    let Some(layout) = guess_layout(osize) else {
                        return null_mut();
                    };
                    // FIXME: Once `allocator_api` is stabilized, use `Global.deallocate`.
                    unsafe { dealloc(ptr, layout) };
                    null_mut()
                } else {
                    let Some(old_layout) = guess_layout(osize) else {
                        return null_mut();
                    };

                    // FIXME: Once `allocator_api` is stabilized, use this.
                    /*
                    let new_layout = guess_layout(nsize);
                    if nsize > osize {
                        Global.grow(alloc_ptr, old_layout, new_layout)
                            .map(|mut ptr| {
                                ptr.as_mut().as_mut_ptr() as *mut c_void
                            })
                            .unwrap_or(null_mut())
                    } else if nsize < osize {
                        Global.shrink(alloc_ptr, old_layout, new_layout)
                            .map(|mut ptr| {
                                ptr.as_mut().as_mut_ptr() as *mut c_void
                            })
                            .unwrap_or(null_mut())
                    } else {
                        ptr
                    }
                    */
                    unsafe { realloc(ptr, old_layout, nsize) as *mut c_void }
                }
            } else if let Some(layout) = guess_layout(nsize) {
                // FIXME: Once `allocator_api` is stabilized, use this.
                /*
                Global.allocate(guess_layout(nsize))
                    .map(|ptr| (*ptr.as_ptr()).as_mut_ptr() as *mut c_void)
                    .unwrap_or(null_mut())
                */
                unsafe { alloc(layout) as *mut c_void }
            } else {
                null_mut()
            }
        }

        unsafe { Self::from_new_ptr(lua_newstate(l_alloc, null_mut())) }
    }

    /// Return the raw pointer to the underlying Lua state.
    pub fn as_ptr(&mut self) -> *mut lua_State {
        self.thread.as_ptr()
    }
}
