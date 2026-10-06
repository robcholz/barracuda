//! Minimal bare-metal symbols required by Rust.

#![allow(unsafe_code)]

// Panic-abort builds still emit references from `.eh_frame` sections. No
// unwinding reaches this symbol on the ESP32-S3 Platform.
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}
