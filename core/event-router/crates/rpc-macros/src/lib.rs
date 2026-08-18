//! Attribute macros for `barracuda-rpc`.
//!
//! [`macro@rpc_json`] marks an `impl RpcMethod` block as reachable through
//! `RpcClient::call_json` by filling the `RpcMethod::json_codec` hook.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, parse_quote, ImplItem, ItemImpl};

/// Makes a typed RPC method reachable through `RpcClient::call_json`.
///
/// Apply it to an `impl RpcMethod for Method` block. It re-emits the block
/// unchanged and appends a `json_codec` override that returns
/// `JsonCodec::of::<Self>()`, so the ordinary `RpcRegistry::register` captures
/// the transcoder beside the endpoint. The method's `Request` must implement
/// `Deserialize` and its `Response` and `Error` must implement `Serialize`;
/// otherwise the generated call fails to compile.
///
/// The expansion references `::barracuda_rpc`, so the annotated crate must
/// depend on `barracuda-rpc` directly.
///
/// ```ignore
/// #[rpc_json]
/// impl RpcMethod for SetPermissionLevel {
///     const ADDRESS: &'static str = "session.set_permission_level";
///     type Request = SetPermissionLevelRequest;
///     type Response = ();
///     type Error = SessionRpcError;
///     type Input = Unary;
///     type Output = Unary;
/// }
/// ```
#[proc_macro_attribute]
pub fn rpc_json(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemImpl);
    let json_codec: ImplItem = parse_quote! {
        fn json_codec() -> ::core::option::Option<::barracuda_rpc::JsonCodec> {
            ::core::option::Option::Some(::barracuda_rpc::JsonCodec::of::<Self>())
        }
    };
    input.items.push(json_codec);
    quote!(#input).into()
}
