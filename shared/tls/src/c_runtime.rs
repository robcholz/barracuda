//! The Rust half of the C library mbedTLS calls on bare-metal targets.
//!
//! `c/runtime.c` forwards `calloc`, `free`, `printf` and `puts` here. They are
//! weak there, so a target's own allocator or console still takes precedence.
#![allow(unsafe_code)]

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use core::ffi::c_char;

/// Bytes kept before each allocation for its size: also the alignment C
/// expects from `calloc`, which is at most 16 on supported targets.
const HEADER: usize = 16;

/// Zeroed allocation of `count * size` bytes, or null.
#[unsafe(no_mangle)]
extern "C" fn barracuda_tls_calloc(count: usize, size: usize) -> *mut u8 {
    let Some(total) = count
        .checked_mul(size)
        .and_then(|bytes| bytes.checked_add(HEADER))
    else {
        return core::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, HEADER) else {
        return core::ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size, since it includes the header.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        return base;
    }
    // SAFETY: the allocation is at least `HEADER` bytes and aligned to
    // `HEADER`, so it can hold the size and returns a pointer inside it.
    unsafe {
        base.cast::<usize>().write(total);
        base.add(HEADER)
    }
}

/// Releases memory from [`barracuda_tls_calloc`]; null is ignored.
///
/// # Safety
///
/// `pointer` is null or came from `barracuda_tls_calloc` and was not freed.
#[unsafe(no_mangle)]
unsafe extern "C" fn barracuda_tls_free(pointer: *mut u8) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: the caller passes a pointer `HEADER` bytes into an allocation
    // whose first word holds its total size.
    unsafe {
        let base = pointer.sub(HEADER);
        let total = base.cast::<usize>().read();
        dealloc(base, Layout::from_size_align_unchecked(total, HEADER));
    }
}

/// Logs one line of mbedTLS console output.
///
/// # Safety
///
/// `text` points to `length` readable bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn barracuda_tls_print(text: *const c_char, length: usize) {
    // SAFETY: the caller passes `length` readable bytes.
    let bytes = unsafe { core::slice::from_raw_parts(text.cast::<u8>(), length) };
    let line = core::str::from_utf8(bytes).unwrap_or("<mbedTLS output is not UTF-8>");
    log::info!(target: "mbedtls", "{}", line.trim_end());
}
