//! Attribute and derive macros for `barracuda-rpc`.
//!
//! [`macro@rpc_dynamic`] marks an `impl RpcMethod` block as runtime-dynamic —
//! reachable through `RpcClient::call_json`, wire-patchable through its
//! [`WireSupport`], and (when a schema is baked) described by
//! `RpcMethod::dynamic`. [`macro@RpcWire`] derives the field→region table each
//! message struct exposes for wire-level field access.

use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use serde_derive_internals::{ast, Ctxt, Derive};
use syn::{parse_macro_input, parse_quote, Data, DeriveInput, Fields, ImplItem, LitStr, Type};

/// Resolves the path to the RPC crate's public items as the caller sees them.
///
/// End users depend only on `barracuda-event-router` (the facade, which
/// re-exports the RPC surface); framework-internal crates depend on
/// `barracuda-rpc` directly. Preferring the facade keeps the expansion working
/// without a direct `barracuda-rpc` dependency.
fn rpc_crate() -> proc_macro2::TokenStream {
    for package in ["barracuda-event-router", "barracuda-rpc"] {
        match crate_name(package) {
            Ok(FoundCrate::Itself) => {
                let ident = format_ident!("{}", package.replace('-', "_"));
                return quote!(::#ident);
            }
            Ok(FoundCrate::Name(name)) => {
                let ident = format_ident!("{name}");
                return quote!(::#ident);
            }
            Err(_) => {}
        }
    }
    quote!(::barracuda_rpc)
}

/// Makes a typed RPC method reachable through the runtime-dynamic surface.
///
/// Apply it to an `impl RpcMethod for Method` block. It re-emits the block and
/// appends one hook override:
///
/// - `dynamic` — returns `Some(Dynamic::new(json, wire, schema))`:
///   - `json` is `JsonCodec::of::<Self>()`, so `RpcClient::call_json` can
///     transcode. The `Request` must implement `Deserialize` and the
///     `Response`/`Error` must implement `Serialize`.
///   - `wire` is `WireSupport::of::<Self>()`, so links can read and write
///     individual fields. Both `Request` and `Response` must implement
///     [`macro@RpcWire`] (`()` already does).
///   - `schema` — under the `rpc_schema_baked` cfg (set by a `build.rs` that
///     runs `barracuda_rpc_schema::bake_all`), the request schema embedded from
///     `$OUT_DIR/<Request>.json`; otherwise `None`.
///
/// The expansion resolves the RPC crate through `barracuda-event-router` (the
/// facade) when present, falling back to `barracuda-rpc`, so an end-user crate
/// that depends only on the facade does not need a direct `barracuda-rpc`
/// dependency.
#[proc_macro_attribute]
pub fn rpc_dynamic(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as syn::ItemImpl);
    let rpc = rpc_crate();

    let Some(request_type) = request_type_name(&input) else {
        return syn::Error::new_spanned(
            &input,
            "#[rpc_dynamic] requires an `impl RpcMethod` block with a `type Request = ...;` item",
        )
        .to_compile_error()
        .into();
    };

    let dynamic: ImplItem = if request_type.is_unit {
        parse_quote! {
            fn dynamic() -> ::core::option::Option<#rpc::Dynamic> {
                ::core::option::Option::Some(#rpc::Dynamic::new(
                    #rpc::JsonCodec::of::<Self>(),
                    #rpc::WireSupport::of::<Self>(),
                    ::core::option::Option::None,
                ))
            }
        }
    } else {
        let file = LitStr::new(
            &format!("{}.json", request_type.schema_name),
            proc_macro2::Span::call_site(),
        );
        parse_quote! {
            #[allow(unexpected_cfgs)]
            fn dynamic() -> ::core::option::Option<#rpc::Dynamic> {
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
                ::core::option::Option::Some(#rpc::Dynamic::new(
                    #rpc::JsonCodec::of::<Self>(),
                    #rpc::WireSupport::of::<Self>(),
                    SCHEMA,
                ))
            }
        }
    };
    input.items.push(dynamic);
    quote!(#input).into()
}

/// Bundles the standard derive set for a fixed-layout RPC message.
///
/// This is an attribute macro rather than a derive because a
/// `proc_macro_derive` cannot inject additional `#[derive(...)]` attributes
/// that the compiler will expand. Applying `#[rpc_message]` to a struct or enum
/// is shorthand for:
///
/// ```ignore
/// #[derive(
///     serde::Serialize,
///     serde::Deserialize,
///     zerocopy::Immutable,
///     zerocopy::IntoBytes,
///     zerocopy::KnownLayout,
///     zerocopy::TryFromBytes,
///     RpcWire, // named-field structs only
/// )]
/// ```
///
/// The consuming crate must depend on `serde` and `zerocopy` directly, and on
/// either `barracuda-event-router` or `barracuda-rpc`. Keep `#[repr(C)]`
/// (structs) or an explicit `#[repr(...)]` (enums) on the item; this macro only
/// adds derives.
#[proc_macro_attribute]
pub fn rpc_message(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let rpc = {
        let mut ident = format_ident!("barracuda_rpc");
        for package in ["barracuda-event-router", "barracuda-rpc"] {
            match crate_name(package) {
                Ok(FoundCrate::Itself) => {
                    ident = format_ident!("{}", package.replace('-', "_"));
                    break;
                }
                Ok(FoundCrate::Name(name)) => {
                    ident = format_ident!("{name}");
                    break;
                }
                Err(_) => {}
            }
        }
        ident
    };

    let named_struct = match &input.data {
        Data::Struct(data) => matches!(&data.fields, Fields::Named(_)),
        _ => false,
    };
    let wire = if named_struct {
        quote!(#rpc::RpcWire)
    } else {
        quote!()
    };

    quote! {
        #[derive(
            serde::Serialize,
            serde::Deserialize,
            zerocopy::Immutable,
            zerocopy::IntoBytes,
            zerocopy::KnownLayout,
            zerocopy::TryFromBytes,
            #wire
        )]
        #input
    }
    .into()
}

/// Derives [`RpcWire`], the per-field byte-region table for a message struct.
///
/// The derive walks the struct's named fields and emits a `const FIELDS` slice
/// mapping each field's JSON name to its `(offset, size)` byte region. Field
/// names honor `#[serde(rename = "...")]` and the container's
/// `#[serde(rename_all = "...")]`, so a link resolving `$previous.output.<name>`
/// matches the JSON name the codec produces. Offsets come from
/// [`core::mem::offset_of!`] and sizes from [`core::mem::size_of`]; both are
/// `const`, so field lookup carries no per-call layout cost.
///
/// Only `struct`s with named fields are supported. `()` has a provided
/// implementation with no fields. Each field's wire name is the JSON name
/// `serde_json` itself computes; a field whose serialize and deserialize names
/// differ (split `#[serde(rename(serialize = ..., deserialize = ...))]`) is
/// rejected, so the table can never disagree with the JSON surface.
#[proc_macro_derive(RpcWire)]
pub fn derive_rpc_wire(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let rpc = rpc_crate();
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Parse serde attributes with serde's own internals so JSON names are
    // computed by the same code `serde_json` uses — no hand-rolled rename
    // logic that could drift from serde's behavior.
    let cx = Ctxt::new();
    let Some(container) = ast::Container::from_ast(&cx, &input, Derive::Serialize) else {
        return syn::Error::new_spanned(&input, "#[derive(RpcWire)] supports only structs")
            .to_compile_error()
            .into();
    };
    if let Err(error) = cx.check() {
        return error.to_compile_error().into();
    }
    let ast::Data::Struct(style, fields) = &container.data else {
        return syn::Error::new_spanned(&input, "#[derive(RpcWire)] supports only structs")
            .to_compile_error()
            .into();
    };
    if !matches!(style, ast::Style::Struct) {
        return syn::Error::new_spanned(
            &input,
            "#[derive(RpcWire)] requires a struct with named fields",
        )
        .to_compile_error()
        .into();
    }

    let mut entries = Vec::new();
    for field in fields {
        // Fields serde cannot address through JSON are not addressable by a
        // Mapping link either.
        if field.attrs.skip_serializing() || field.attrs.skip_deserializing() {
            continue;
        }
        let syn::Member::Named(ident) = &field.member else {
            continue;
        };
        let ty = field.ty;
        let serialize_name = field.attrs.name().serialize_name();
        if serialize_name != field.attrs.name().deserialize_name() {
            return syn::Error::new_spanned(
                field.original,
                "RpcWire requires a field's serialize and deserialize JSON names to match",
            )
            .to_compile_error()
            .into();
        }
        let json_name = LitStr::new(serialize_name, ident.span());
        entries.push(quote! {
            #rpc::WireField::new(
                #json_name,
                ::core::mem::offset_of!(Self, #ident),
                ::core::mem::size_of::<#ty>(),
            )
        });
    }

    quote! {
        impl #impl_generics #rpc::RpcWire for #name #ty_generics #where_clause {
            const FIELDS: &'static [#rpc::WireField] = &[#(#entries),*];
        }
    }
    .into()
}

/// How the macro classifies the `type Request = ...;` associated type.
struct RequestType {
    /// Final path segment of the request type, used for the baked schema file.
    schema_name: String,
    /// The request is the unit type `()`, which has no schema to bake.
    is_unit: bool,
}

/// Classifies the `type Request = ...;` associated type of an `impl RpcMethod`
/// block.
fn request_type_name(input: &syn::ItemImpl) -> Option<RequestType> {
    input.items.iter().find_map(|item| {
        let ImplItem::Type(assoc) = item else {
            return None;
        };
        if assoc.ident != "Request" {
            return None;
        }
        match &assoc.ty {
            Type::Path(path) => path.path.segments.last().map(|segment| RequestType {
                schema_name: segment.ident.to_string(),
                is_unit: false,
            }),
            Type::Tuple(tuple) if tuple.elems.is_empty() => Some(RequestType {
                schema_name: String::from("()"),
                is_unit: true,
            }),
            _ => None,
        }
    })
}
