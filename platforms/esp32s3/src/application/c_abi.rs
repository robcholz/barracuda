//! Minimal bare-metal symbols required by Rust and vendored C libraries.

#![allow(unsafe_code)]

use core::ffi::{c_int, c_long, c_void};

const ERROR: c_int = -1;

// Panic-abort builds still emit references from `.eh_frame` sections. No
// unwinding reaches this symbol on the ESP32-S3 Platform.
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}

#[unsafe(no_mangle)]
extern "C" fn _close(_file: c_int) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _fcntl(_file: c_int, _command: c_int) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _exit(_status: c_int) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "C" fn _fstat(_file: c_int, _status: *mut c_void) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _getpid() -> c_int {
    1
}

#[unsafe(no_mangle)]
extern "C" fn _gettimeofday(_time: *mut c_void, _timezone: *mut c_void) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _isatty(_file: c_int) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn _kill(_process: c_int, _signal: c_int) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _lseek(_file: c_int, _offset: c_long, _origin: c_int) -> c_long {
    -1
}

#[unsafe(no_mangle)]
extern "C" fn _open(_path: *const u8, _flags: c_int, _mode: c_int) -> c_int {
    ERROR
}

#[unsafe(no_mangle)]
extern "C" fn _read(_file: c_int, _buffer: *mut u8, _length: usize) -> isize {
    -1
}

#[unsafe(no_mangle)]
extern "C" fn _sbrk(_increment: isize) -> *mut c_void {
    usize::MAX as *mut c_void
}

#[unsafe(no_mangle)]
extern "C" fn _times(_buffer: *mut c_void) -> c_long {
    -1
}

#[unsafe(no_mangle)]
extern "C" fn _write(_file: c_int, _buffer: *const u8, _length: usize) -> isize {
    -1
}

#[unsafe(no_mangle)]
extern "C" fn __getreent() -> *mut c_void {
    core::ptr::null_mut()
}
