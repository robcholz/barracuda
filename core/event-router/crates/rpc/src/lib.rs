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
pub use context::{RpcCallId, RpcContext, RpcEndpointId};
pub use frame::RpcFrame;
pub use json::{
    JsonCall, JsonHandler, JsonHandlerFuture, JsonObjectFields, JsonObjectPayload,
    JsonObjectWriter, JsonPayload, JsonRef, JsonRpcInfo, JsonRpcSchema, JsonSchema, JsonWriter,
};
pub use lane::RpcLaneStorage;
pub use payload::{
    RpcMulticastBranch, RpcPayloadFrame, RpcPayloadReader, RpcPayloadWriteFrame, RpcPayloadWriter,
};
pub(crate) use registry::RpcDirection;
pub use registry::{RpcClient, RpcError, RpcMethodInfo, RpcRegistration, RpcRegistry, RpcResult};
pub use registry::{RpcEndpoint, RpcRegistryApi};
pub use typed::{
    RpcCardinality, RpcHandler, RpcHandlerFuture, RpcHandlerInput, RpcHandlerOutput, RpcInputMode,
    RpcMessage, RpcMethod, RpcMethodDescriptor, RpcOutputMode, RpcStream, RpcUnaryCall, Streaming,
    Unary,
};
