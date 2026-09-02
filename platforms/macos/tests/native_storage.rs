//! Shared behavioral contract for macOS native flash implementations.

#![allow(clippy::expect_used, clippy::panic)]

include!("../../native_storage_contract.rs");

native_storage_contract!(native_storage_contract, barracuda_platform_macos);
