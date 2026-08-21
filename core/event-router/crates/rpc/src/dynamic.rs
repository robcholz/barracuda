//! Runtime-dynamic surface bundled onto a typed RPC method.
//!
//! A method that opts into runtime dispatch always opts into all three of it at
//! once: JSON transcoding, wire-level field access, and a request schema. They
//! are produced together by `#[rpc_dynamic]` and stored together beside the
//! endpoint, so [`RpcMethod::dynamic`](crate::RpcMethod::dynamic) returns one
//! [`Dynamic`] rather than three separate hooks.

use getset::{CopyGetters, Getters};

use super::json::JsonCodec;
use super::wire::WireSupport;

/// The runtime-dynamic capabilities of one [`RpcMethod`](crate::RpcMethod).
///
/// Built by `#[rpc_dynamic]` through [`Dynamic::new`] and returned from
/// [`RpcMethod::dynamic`](crate::RpcMethod::dynamic). Cloning is cheap: the
/// codec shares an `Rc`, and the wire tables and schema are `'static`.
#[derive(Clone, Debug, Getters, CopyGetters)]
pub struct Dynamic {
    /// The JSON transcoder.
    #[getset(get = "pub")]
    json: JsonCodec,
    /// The wire-level field tables.
    #[getset(get = "pub")]
    wire: WireSupport,
    /// The baked request schema, when the schema pipeline ran.
    #[getset(get_copy = "pub")]
    schema: Option<&'static str>,
}

impl Dynamic {
    /// Bundles the codec, wire tables, and optional schema for one method.
    ///
    /// `#[rpc_dynamic]` emits this call; write it by hand only to fill
    /// [`RpcMethod::dynamic`](crate::RpcMethod::dynamic) without the attribute.
    #[must_use]
    pub fn new(json: JsonCodec, wire: WireSupport, schema: Option<&'static str>) -> Self {
        Self { json, wire, schema }
    }
}
