/// Push a formatted string with [`lua_pushfstring`](crate::cdef::lua_pushfstring).
///
/// The "signature" for this function is
/// `lua_push_fmt_string!(lua: &Thread, fmt: <string>, ...)`, where `fmt` is a
/// literal format string, and `...` are the format arguments.
///
/// It is similar to the ISO C function `sprintf`, however you do not have to
/// allocate space for the result; the result is a Lua string and Lua takes care
/// of memory allocation (and deallocation, through garbage collection).
///
///	# Conversion specifiers
/// There are no flags, widths, or precisions.
/// The conversion specifiers can only be:
/// - `%%` - insert the character `%`.
/// - `%s` - insert a zero-terminated string using [`*const c_char`](core::ffi::c_char), with no size restrictions.
/// - `%f` - insert a [`Number`](crate::cdef::Number).
/// - `%I` - insert an [`Integer`](crate::cdef::Integer).
/// - `%p` - insert a *thin* pointer, like [`*mut c_void`](core::ffi::c_void).
/// - `%d` - insert a [`c_int`](core::ffi::c_int).
/// - `%c` - insert a [`c_int`](core::ffi::c_int) as a one-byte character.
/// - `%U` - insert a [`c_long`](core::ffi::c_long) as a UTF-8 byte sequence.
///
/// # Safety
/// The macro uses an unsafe function, and is itself unsafe to use; there have
/// to be sufficient format arguments, and they must be of the correct type.
#[macro_export]
macro_rules! push_fmt_string {
	($lua:expr, $fmt:expr $(, $($fmt_arg:tt)*)?) => {{
		let lua: &mut $crate::Thread = &mut $lua;
		let fmt: &::core::ffi::CStr = $fmt;
		$crate::cdef::lua_pushfstring(lua.as_ptr(), fmt.as_ptr()$(, $($fmt_arg)*)?)
	}};
}

/// Raise a formatted error with [`luaL_error`](crate::cdef::auxlib::luaL_error).
///
/// The "signature" for this function is
/// `lua_fmt_error!(lua: &Thread, fmt: <string>, ...)`, where `fmt` is a
/// literal format string, and `...` are the format arguments.
///
/// This function follows the same rules as [`push_fmt_string!`].
///
/// # Safety
/// The macro uses an unsafe function, and is itself unsafe to use; there have
/// to be sufficient format arguments, and they must be of the correct type.
#[macro_export]
macro_rules! fmt_error {
	($lua:expr, $fmt:literal $(, $fmt_arg:expr)*) => {{
		let lua: &$crate::Thread = &$lua;
		$crate::cdef::auxlib::luaL_error(
			lua.as_ptr(),
			core::ffi::CStr::from_bytes_with_nul_unchecked(
				concat!($fmt, "\0").as_bytes()
			).as_ptr()
			$(, $fmt_arg)*
		)
	}};
}
