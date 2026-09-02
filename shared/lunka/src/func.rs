use core::{
    ffi::c_int,
    marker::PhantomData,
    mem::transmute,
    ops::{Deref, DerefMut},
};

use crate::{
    Managed, Thread,
    cdef::{lua_CFunction, lua_State},
};

impl Thread {
    /// Set a new panic function and return the old one.
    ///
    /// This returns an optional [`lua_CFunction`] - not [`Func`].
    pub fn at_panic(&self, func: Option<Func>) -> Option<lua_CFunction> {
        self.at_c_panic(func.map(to_c_function))
    }

    /// Push a light function onto the stack (that is, a function with no upvalues).
    pub fn push_function(&self, f: Func) {
        self.push_c_function(to_c_function(f))
    }
}

impl Managed<'_> {
    /// Push a new closure onto the stack.
    ///
    /// See [`Managed::push_c_closure`] for more information.
    pub fn push_closure(&mut self, f: Func, n_upvalues: c_int) {
        self.push_c_closure(to_c_function(f), n_upvalues)
    }
}

/// Rust function that can be called by Lua.
///
/// This type is compatible with [`lua_CFunction`];
/// however, this may not be a safe assumption to make for the reverse.
/// Use [`to_c_function`] to convert from this type if needed.
pub type Func = extern "C-unwind" fn(Ctx<'_>) -> Rets;

/// Converts a [`Func`] to [`lua_CFunction`].
pub const fn to_c_function(f: Func) -> lua_CFunction {
    unsafe { transmute(f) }
}

/// Context passed to a [`Func`].
///
/// This type [`Deref`]s to a [`Thread`],
/// and can also be converted to `&'a mut Thread`.
///
/// # Layout
/// This type has the same layout and ABI as [`*mut lua_State`](lua_State).
#[repr(transparent)]
pub struct Ctx<'a> {
    l: *mut lua_State,
    _thread: PhantomData<&'a mut Thread>,
}

impl Deref for Ctx<'_> {
    type Target = Thread;
    fn deref(&self) -> &Self::Target {
        unsafe { Thread::from_ptr(self.l) }
    }
}
impl DerefMut for Ctx<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { Thread::from_ptr_mut(self.l) }
    }
}

impl<'a> Ctx<'a> {
    /// Converts this context into [`Thread`].
    pub fn lua(self) -> &'a mut Thread {
        unsafe { Thread::from_ptr_mut(self.l) }
    }
}

/// Type for the number of values returned from a [`Func`].
///
/// # Layout
/// This type has the same layout and ABI as [`c_int`].
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Rets {
    // INVARIANT: This field is non-negative.
    count: c_int,
}

impl Rets {
    /// Constant that indicates no return values.
    pub const ZERO: Self = Self::new(0);
    /// Constant that indicates one return value.
    pub const ONE: Self = Self::new(1);

    /// Creates a new [`Rets`] from the specified number of returned values.
    pub const fn new(n: c_int) -> Self {
        if !n.is_negative() {
            unsafe { Self::new_unchecked(n) }
        } else {
            unsafe { Self::new_unchecked(0) }
        }
    }

    /// Creates a new [`Rets`] from the specified number of returned values.
    pub const fn from_usize(n: usize) -> Self {
        let count = if n <= c_int::MAX as usize {
            n as c_int
        } else {
            0
        };
        unsafe { Self::new_unchecked(count) }
    }

    /// Creates a new [`Rets`] without checking whether
    /// the number of returned values is non-negative.
    ///
    /// # Safety
    /// `count` must be non-negative.
    pub const unsafe fn new_unchecked(count: c_int) -> Self {
        Self { count }
    }
}

impl From<c_int> for Rets {
    fn from(value: c_int) -> Self {
        Self::new(value)
    }
}
impl From<usize> for Rets {
    fn from(value: usize) -> Self {
        Self::from_usize(value)
    }
}
impl From<()> for Rets {
    fn from(_: ()) -> Self {
        Self::ZERO
    }
}

/// Return an unnamed function that conforms to
/// the signature of [`Func`].
///
/// The function is not a closure; it may not capture any variables.
#[macro_export]
macro_rules! func {
	($lua:pat => {$($t:tt)*}) => {{
		extern "C-unwind" fn __lua_func_inner(cx: $crate::Ctx<'_>) -> $crate::Rets {
			let $lua = cx.lua();
			<$crate::Rets as ::core::convert::From<_>>::from((move || {$($t)*})())
		}
		__lua_func_inner
	}};

	{$($whatever:tt)*} => {
		::core::compile_error! {
			"expected `<pattern> => { <body> }`"
		}
	};
}

/// Panic function that delegates to [`panic!`].
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub extern "C-unwind" fn rust_panic_handler(mut lua: Ctx<'_>) -> Rets {
    let mut lua = lua.managed();
    let msg = if let Some(msg) = lua.to_string(-1) {
        str::from_utf8(msg).unwrap_or("error object does not contain valid UTF-8")
    } else {
        "error object is not a string"
    };
    panic!("unprotected error in call to Lua API ({msg})")
}
