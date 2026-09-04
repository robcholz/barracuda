//! Attribute macros for `barracuda-rpc`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Bundles the standard derive set for a fixed-layout RPC message.
///
/// This is an attribute macro rather than a derive because a
/// `proc_macro_derive` cannot inject additional `#[derive(...)]` attributes
/// that the compiler will expand. Applying `#[rpc_message]` to a struct or enum
/// is shorthand for:
///
/// ```ignore
/// #[derive(
///     zerocopy::Immutable,
///     zerocopy::IntoBytes,
///     zerocopy::KnownLayout,
///     zerocopy::TryFromBytes,
/// )]
/// ```
///
/// The consuming crate must depend on `zerocopy` directly. Keep `#[repr(C)]`
/// (structs) or an explicit `#[repr(...)]` (enums) on the item; this macro only
/// adds derives.
#[proc_macro_attribute]
pub fn rpc_message(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    quote! {
        #[derive(
            zerocopy::Immutable,
            zerocopy::IntoBytes,
            zerocopy::KnownLayout,
            zerocopy::TryFromBytes,
        )]
        #input
    }
    .into()
}
