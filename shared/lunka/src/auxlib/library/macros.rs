/// Returns an [`ArrayLibrary`](super::ArrayLibrary) initialized
/// from [`lua_CFunction`](crate::cdef::lua_CFunction)s
/// with a record-like syntax.
#[macro_export]
macro_rules! library_c {
	{} => {
		$crate::ArrayLibrary::empty()
	};

	{$($field:ident $(: $fn:expr)?),+ $(,)?} => {
		$crate::ArrayLibrary::from_c_functions([
			$($crate::library!(@field $field $($fn)?),)+
		])
	};

	(@field $field:ident $fn:expr) => {
		($crate::library!(@field_str $field), Some($fn))
	};

	(@field $field:ident) => {
		($crate::library!(@field_str $field), None)
	};

	(@field_str $field:ident) => {
		unsafe { ::core::ffi::CStr::from_bytes_with_nul_unchecked(
			::core::concat! { ::core::stringify!($field), '\0' }.as_bytes()
		) }
	};

	{$($whatever:tt)*} => {
		::core::compile_error! {
			"expected a `,`-delimited list of `<field> [: <function>]`"
		}
	};
}

/// Returns an [`ArrayLibrary`](super::ArrayLibrary) initialized
/// from [`Func`](crate::Func)s
/// with a record-like syntax.
#[macro_export]
macro_rules! library_a {
	{} => {
		$crate::ArrayLibrary::empty()
	};

	{$($field:ident $(: $fn:expr)?),+ $(,)?} => {
		$crate::ArrayLibrary::from_funcs([
			$($crate::library_a!(@field $field $($fn)?),)+
		])
	};

	(@field $field:ident $fn:expr) => {
		($crate::library_a!(@field_str $field), Some($fn))
	};

	(@field $field:ident) => {
		($crate::library_a!(@field_str $field), None)
	};

	(@field_str $field:ident) => {
		unsafe { ::core::ffi::CStr::from_bytes_with_nul_unchecked(
			::core::concat! { ::core::stringify!($field), '\0' }.as_bytes()
		) }
	};

	{$($whatever:tt)*} => {
		::core::compile_error! {
			"expected a `,`-delimited list of `<field> [: <function>]`"
		}
	};
}

/// Returns a [`StaticLibrary`](super::StaticLibrary) initialized
/// from [`Func`](crate::Func)s
/// with a record-like syntax.
#[macro_export]
macro_rules! library {
	{$($t:tt)*} => {
		$crate::StaticLibrary::from_array(&$crate::library_a! {$($t)*})
	};
}
