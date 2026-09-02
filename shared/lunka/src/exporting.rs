use crate::{Ctx, Rets, Thread};

/// Unstable implementation detail for the macros exported by this crate.
#[allow(non_snake_case)]
#[doc(hidden)]
pub fn __impl_call_entrypoint<F: ?Sized + Fn(&mut Thread) -> R, R: Into<Rets>>(
    cx: Ctx<'_>,
    f: &F,
) -> Rets {
    f(cx.lua()).into()
}

/// Exports the given closure with the signature `fn(&mut Thread) -> Rets`
/// as a Lua library entrypoint.
#[macro_export]
macro_rules! export_fn {
	{
		@export
		$m:tt
		$entrypoint:ident
		$($t:tt)*
	} => {
		const _: () = {
			fn lunka_export_fn_impl_do_entrypoint(cx: $crate::Ctx<'_>) -> $crate::Rets {
				$crate::__impl_call_entrypoint(cx, &$($t)*)
			}
			const _: $crate::Func = {
				# $m
				extern "C-unwind" fn $entrypoint(cx: $crate::Ctx<'_>) -> $crate::Rets {
					lunka_export_fn_impl_do_entrypoint(cx)
				}
				$entrypoint
			};
		};
	};

	($entrypoint_fn:ident) => {
		$crate::export_fn! {
			@export
			[unsafe(no_mangle)]
			$entrypoint_fn
			$entrypoint_fn
		}
	};

	($entrypoint:ident => $($t:tt)*) => {
		$crate::export_fn! {
			@export
			[unsafe(no_mangle)]
			$entrypoint
			$($t)*
		}
	};
	($entrypoint:expr => $($t:tt)*) => {
		$crate::export_fn! {
			@export
			[unsafe(export_name = $entrypoint)]
			lunka_export_fn_impl_entrypoint
			$($t)*
		}
	};

	{$($whatever:tt)*} => {
		::core::compile_error! {
			"expected either `<export name> => <function>` or `<function name>`"
		}
	};
}
