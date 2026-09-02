//! FFI definitions for `lualib.h`.

use super::*;

use core::ffi::CStr;

pub const COROUTINE_LIB_NAME: &CStr = c"coroutine";
pub const TABLE_LIB_NAME: &CStr = c"table";
pub const IO_LIB_NAME: &CStr = c"io";
pub const OS_LIB_NAME: &CStr = c"os";
pub const STRING_LIB_NAME: &CStr = c"string";
pub const UTF8_LIB_NAME: &CStr = c"utf8";
pub const MATH_LIB_NAME: &CStr = c"math";
pub const DEBUG_LIB_NAME: &CStr = c"debug";
pub const PACKAGE_LIB_NAME: &CStr = c"package";

#[cfg_attr(
    all(
        feature = "link-system",
        feature = "link-dynamic",
        target_os = "windows"
    ),
    link(name = "lua54", kind = "raw-dylib")
)]
#[cfg_attr(
    all(
        feature = "link-system",
        feature = "link-dynamic",
        not(target_os = "windows")
    ),
    link(name = "lua5.4", kind = "dylib")
)]
#[cfg_attr(
    all(
        feature = "link-system",
        not(feature = "link-dynamic"),
        target_os = "windows"
    ),
    link(name = "lua54", kind = "static")
)]
#[cfg_attr(
    all(
        feature = "link-system",
        not(feature = "link-dynamic"),
        not(target_os = "windows")
    ),
    link(name = "lua5.4", kind = "static")
)]
unsafe extern "C-unwind" {
    lua_state_func! {
        pub fn luaopen_base(self) -> c_int;

        pub fn luaopen_coroutine(self) -> c_int;
        pub fn luaopen_table(self) -> c_int;
        pub fn luaopen_io(self) -> c_int;
        pub fn luaopen_os(self) -> c_int;
        pub fn luaopen_string(self) -> c_int;
        pub fn luaopen_utf8(self) -> c_int;
        pub fn luaopen_math(self) -> c_int;
        pub fn luaopen_debug(self) -> c_int;
        pub fn luaopen_package(self) -> c_int;

        pub fn luaL_openlibs(self);
    }
}
