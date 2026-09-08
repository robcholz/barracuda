#![no_std]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

#[cfg(feature = "tinyrlibc")]
compile_error!("Barracuda's littlefs2-sys fork does not vendor the optional tinyrlibc feature");

#[cfg(not(target_arch = "wasm32"))]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(target_arch = "wasm32")]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/wasm32-wasip1/bindings.rs"
));
