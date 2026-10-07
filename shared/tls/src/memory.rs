//! Where mbedTLS allocates.
//!
//! Each connection's record buffers (about 16 KiB each way) and the server's
//! certificate copies are plain bytes, and the largest allocations mbedTLS
//! makes. They go to the Platform's bulk-memory domain, which is external RAM
//! where a Board has it. Everything smaller, including every structure that
//! holds pointers, stays on the global heap.
#![allow(unsafe_code)]

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use core::ffi::c_void;

use barracuda_bulk_memory::foreign;
use mbedtls_rs::sys::mbedtls_platform_set_calloc_free;

/// Allocations of at least this many bytes go to bulk memory.
const BULK_FROM: usize = 4096;

/// Bytes before each allocation: where it came from and its size, padded to the
/// 16-byte alignment C expects from `calloc`.
const HEADER: usize = 16;

const GLOBAL: usize = 0;
const BULK: usize = 1;

/// Routes every later mbedTLS allocation through [`calloc`] and [`free`].
///
/// Call it before mbedTLS allocates anything, since memory must be freed by
/// the allocator that provided it.
pub(crate) fn install() {
    // SAFETY: both functions follow the C `calloc` and `free` contracts.
    let _always_zero = unsafe { mbedtls_platform_set_calloc_free(Some(calloc), Some(free)) };
}

/// mbedTLS's `calloc`: zeroed `count * size` bytes aligned to 16, or null.
unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count
        .checked_mul(size)
        .and_then(|bytes| bytes.checked_add(HEADER))
    else {
        return core::ptr::null_mut();
    };
    let (base, domain) = if total - HEADER >= BULK_FROM {
        (foreign::calloc(total), BULK)
    } else {
        let Ok(layout) = Layout::from_size_align(total, HEADER) else {
            return core::ptr::null_mut();
        };
        // SAFETY: the layout has a non-zero size, since it includes the header.
        (unsafe { alloc_zeroed(layout) }, GLOBAL)
    };
    if base.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the block is at least `HEADER` bytes and 16-byte aligned, which
    // fits two `usize` values at its start.
    unsafe {
        base.cast::<usize>().write(domain);
        base.add(size_of::<usize>()).cast::<usize>().write(total);
        base.add(HEADER).cast()
    }
}

/// mbedTLS's `free`.
unsafe extern "C" fn free(pointer: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: `calloc` wrote the header just before `pointer`, and mbedTLS
    // frees each allocation once.
    unsafe {
        let base = pointer.cast::<u8>().sub(HEADER);
        let domain = base.cast::<usize>().read();
        let total = base.add(size_of::<usize>()).cast::<usize>().read();
        if domain == BULK {
            foreign::free(base);
        } else {
            dealloc(base, Layout::from_size_align_unchecked(total, HEADER));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{calloc, free, BULK_FROM};

    #[test]
    fn small_and_large_allocations_are_zeroed_aligned_and_freed() {
        for size in [1, 300, BULK_FROM - 1, BULK_FROM, 16_717] {
            // SAFETY: the buffer is used within its size and freed once.
            unsafe {
                let buffer = calloc(size, 1).cast::<u8>();
                assert!(!buffer.is_null());
                assert_eq!(buffer as usize % 16, 0);
                let bytes = core::slice::from_raw_parts_mut(buffer, size);
                assert!(bytes.iter().all(|byte| *byte == 0));
                bytes.fill(0x5a);
                free(buffer.cast());
            }
        }
        // SAFETY: an overflowing request and a null free are both rejected.
        unsafe {
            assert!(calloc(usize::MAX, 2).is_null());
            free(core::ptr::null_mut());
        }
    }
}
