//! Request DTOs shared by the schema-bake example and its `build.rs`.
//!
//! With the `schema` feature the crate is host-only: it derives
//! `schemars::JsonSchema` and submits each type with `register!`. Without the
//! feature it stays `no_std`, so the same DTOs can ship to the device.

#![cfg_attr(not(feature = "schema"), no_std)]

#[repr(C)]
#[barracuda_rpc::rpc_message]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy)]
pub struct SetLevelRequest {
    pub session: u32,
    pub level: u32,
}

#[cfg(feature = "schema")]
barracuda_rpc_schema::register!(SetLevelRequest);
