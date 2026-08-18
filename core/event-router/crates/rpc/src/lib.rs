//! Task-local registry over fixed-capacity full-duplex RPC lanes.
//!
//! [`RpcClient::call`] transfers fixed-layout Zerocopy request, response, and
//! method-error messages through bounded frames. An outer [`RpcResult`] reports
//! transport/runtime failures, while each [`RpcMethod`] exposes its own typed
//! `Result<Response, Error>` outcome.
//!
//! Single-target and request-side multicast entry points share one wire-call
//! state machine. [`RpcClient::call_payload`] returns its asynchronous
//! [`RpcPayloadWriter`] / [`RpcPayloadReader`] handles directly;
//! [`RpcClient::call`] adds fixed-layout encoding and zero-copy typed decoding.
//! [`RpcClient::multicast_payload`] and [`RpcClient::multicast`] reuse the same
//! writer while returning one independent [`RpcMulticastBranch`] per target.

#![no_std]

extern crate alloc;

mod address;
mod context;
mod frame;
mod json;
mod lane;
mod payload;
mod registry;
mod typed;

pub use address::{RpcAddress, RpcAddressError, RpcGroup, RpcGroupError};
/// Marks an `impl RpcMethod` block as reachable through
/// [`RpcClient::call_json`], filling [`RpcMethod::json_codec`].
pub use barracuda_rpc_macros::rpc_json;
pub use context::{RpcCallId, RpcContext, RpcEndpointId};
pub use frame::RpcFrame;
pub use json::JsonCodec;
pub use lane::RpcLaneStorage;
pub use payload::{
    RpcMulticastBranch, RpcPayloadFrame, RpcPayloadReader, RpcPayloadWriteFrame, RpcPayloadWriter,
};
pub(crate) use registry::RpcDirection;
pub use registry::{RpcClient, RpcError, RpcRegistration, RpcRegistry, RpcResult};
pub use registry::{RpcEndpoint, RpcRegistryApi};
pub use typed::{
    RpcHandler, RpcHandlerFuture, RpcHandlerInput, RpcHandlerOutput, RpcInputMode, RpcMessage,
    RpcMethod, RpcOutputMode, RpcStream, RpcUnaryCall, Streaming, Unary,
};
