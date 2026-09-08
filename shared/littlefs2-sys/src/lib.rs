#![no_std]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

#[cfg(feature = "tinyrlibc")]
compile_error!("Barracuda's littlefs2-sys fork does not vendor the optional tinyrlibc feature");

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
