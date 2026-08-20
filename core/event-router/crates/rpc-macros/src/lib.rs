//! Attribute macros for `barracuda-rpc`.
//!
//! [`macro@rpc_json`] marks an `impl RpcMethod` block as reachable through
//! `RpcClient::call_json` and, when a schema is baked, exposes it through
//! `RpcMethod::schema`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, parse_quote, ImplItem, LitStr, Type};

/// Makes a typed RPC method reachable through `RpcClient::call_json` and gives
/// it a build-time schema.
///
/// Apply it to an `impl RpcMethod for Method` block. It re-emits the block and
/// appends two hook overrides:
///
/// - `json_codec` — returns `JsonCodec::of::<Self>()`, so `RpcRegistry::register`
///   captures the transcoder. The `Request` must implement `Deserialize` and the
///   `Response`/`Error` must implement `Serialize`.
/// - `schema` — under the `rpc_schema_baked` cfg (set by a `build.rs` that runs
///   `barracuda_rpc_schema::bake_all`), returns the request schema embedded from
///   `$OUT_DIR/<Request>.json`; otherwise `None`.
///
/// The expansion references `::barracuda_rpc`, so the annotated crate must
/// depend on `barracuda-rpc` directly.
#[proc_macro_attribute]
pub fn rpc_json(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as syn::ItemImpl);

    let json_codec: ImplItem = parse_quote! {
        fn json_codec() -> ::core::option::Option<::barracuda_rpc::JsonCodec> {
            ::core::option::Option::Some(::barracuda_rpc::JsonCodec::of::<Self>())
        }
    };
    input.items.push(json_codec);

    match request_type_name(&input) {
        Some(name) => {
            let file = LitStr::new(&format!("{name}.json"), proc_macro2::Span::call_site());
            let schema: ImplItem = parse_quote! {
                #[allow(unexpected_cfgs)]
                fn schema() -> ::core::option::Option<&'static str> {
                    #[cfg(rpc_schema_baked)]
                    const SCHEMA: ::core::option::Option<&'static str> =
                        ::core::option::Option::Some(::core::include_str!(::core::concat!(
                            ::core::env!("OUT_DIR"),
                            "/",
                            #file
                        )));
                    #[cfg(not(rpc_schema_baked))]
                    const SCHEMA: ::core::option::Option<&'static str> =
                        ::core::option::Option::None;
                    SCHEMA
                }
            };
            input.items.push(schema);
            quote!(#input).into()
        }
        None => syn::Error::new_spanned(
            &input,
            "#[rpc_json] requires an `impl RpcMethod` block with a `type Request = ...;` item",
        )
        .to_compile_error()
        .into(),
    }
}

/// Extracts the final path segment of the `type Request = ...;` associated type.
fn request_type_name(input: &syn::ItemImpl) -> Option<String> {
    input.items.iter().find_map(|item| {
        let ImplItem::Type(assoc) = item else {
            return None;
        };
        if assoc.ident != "Request" {
            return None;
        }
        let Type::Path(path) = &assoc.ty else {
            return None;
        };
        path.path.segments.last().map(|s| s.ident.to_string())
    })
}
