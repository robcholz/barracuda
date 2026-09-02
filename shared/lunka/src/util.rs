use core::ffi::{CStr, c_char};

pub const unsafe fn opt_c_str<'a>(ptr: *const c_char) -> Option<&'a CStr> {
    if !ptr.is_null() {
        unsafe { Some(CStr::from_ptr(ptr)) }
    } else {
        None
    }
}
