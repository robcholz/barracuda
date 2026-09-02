//! FFI definitions for `lauxlib.h`.

#![allow(non_snake_case)]

use super::*;

use core::{
    ffi::CStr,
    mem::size_of,
    ptr::{null, null_mut},
};

/// Global table name.
///
/// This cannot be changed for Lua that's already compiled.
pub const GLOBAL_TABLE: &CStr = c"_G";

/// Key, in the registry, for the table of loaded modules.
///
/// This cannot be changed for Lua that's already compiled.
pub const LOADED_TABLE: &CStr = c"_LOADED";

/// Key, in the registry, for the table of preloaded loaders.
///
/// This cannot be changed for Lua that's already compiled.
pub const PRELOAD_TABLE: &CStr = c"_PRELOAD";

/// Type for arrays of functions to be registered by [`luaL_setfuncs`].
///
/// [`luaL_Reg::name`] is the function name and
/// [`luaL_Reg::func`] is a pointer to the function.
///
/// Any array of [`luaL_Reg`] must end with a sentinel entry in which both `name` and
/// `func` are null.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct luaL_Reg {
    pub name: *const c_char,
    pub func: Option<lua_CFunction>,
}

impl luaL_Reg {
    /// Sentinel entry which marks the end of a list of [`luaL_Reg`]s.
    pub const NULL: Self = Self {
        name: null(),
        func: None,
    };
}

/// Packed combination [`Integer`] and [`Number`] type sizes for
/// [`luaL_checkversion_`] to check against.
pub const NUM_SIZES: usize = size_of::<Integer>() * 16 + size_of::<Number>();

/// Constant that is guaranteed to indicate no reference.
pub const NO_REF: c_int = -2;

/// Constant that indicates a reference to `nil`.
pub const REF_NIL: c_int = -1;

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
    pub fn luaL_newstate() -> *mut lua_State;

    pub fn luaL_prepbuffsize(buffer: *mut luaL_Buffer, size: usize) -> *mut c_char;
    pub fn luaL_addlstring(buffer: *mut luaL_Buffer, str: *const c_char, len: usize);
    pub fn luaL_addstring(buffer: *mut luaL_Buffer, str: *const c_char);
    pub fn luaL_addvalue(buffer: *mut luaL_Buffer);
    pub fn luaL_pushresult(buffer: *mut luaL_Buffer);
    pub fn luaL_pushresultsize(buffer: *mut luaL_Buffer, size: usize);

    lua_state_func! {
        pub fn luaL_checkversion_(self, ver: Number, sz: usize);
        pub fn luaL_getmetafield(
            self, obj: c_int, e: *const c_char
        ) -> c_int;
        pub fn luaL_callmeta(self, obj: c_int, e: *const c_char) -> c_int;
        pub fn luaL_tolstring(
            self, idx: c_int, len: *mut usize
        ) -> *const c_char;
        pub fn luaL_argerror(self, arg: c_int, extra_msg: *const c_char) -> !;
        pub fn luaL_typeerror(self, arg: c_int, type_name: *const c_char) -> !;
        pub fn luaL_checklstring(
            self, arg: c_int, len: *mut usize
        ) -> *const c_char;
        pub fn luaL_optlstring(
            self, arg: c_int, default: *const c_char, len: *mut usize
        ) -> *const c_char;
        pub fn luaL_checknumber(self, arg: c_int) -> Number;
        pub fn luaL_optnumber(self, arg: c_int, default: Number) -> Number;

        pub fn luaL_checkinteger(self, arg: c_int) -> Integer;
        pub fn luaL_optinteger(
            self, arg: c_int, default: Integer
        ) -> Integer;

        pub fn luaL_checkstack(self, sz: c_int, msg: *const c_char);
        pub fn luaL_checktype(self, arg: c_int, type_tag: c_int);
        pub fn luaL_checkany(self, arg: c_int);

        pub fn luaL_newmetatable(self, type_name: *const c_char) -> c_int;
        pub fn luaL_setmetatable(self, type_name: *const c_char);
        pub fn luaL_testudata(
            self, ud: c_int, type_name: *const c_char
        ) -> *mut c_void;
        pub fn luaL_checkudata(
            self, ud: c_int, type_name: *const c_char
        ) -> *mut c_void;

        pub fn luaL_where(self, level: c_int);
        /// # Note
        /// The return type should be [`c_int`] judging from the C header,
        /// however the documentation states that this function *never* returns.
        ///
        /// See the manual for more information:
        /// <https://www.lua.org/manual/5.4/manual.html#luaL_error>
        pub fn luaL_error(self, fmt: *const c_char, ...) -> !;

        pub fn luaL_checkoption(
            self, arg: c_int, default: *const c_char,
            list: *const *const c_char
        ) -> c_int;

        pub fn luaL_fileresult(
            self, status: c_int, file_name: *const c_char
        ) -> c_int;
        pub fn luaL_execresult(self, status: c_int) -> c_int;

        pub fn luaL_ref(self, table: c_int) -> c_int;
        pub fn luaL_unref(self, table: c_int, ref_idx: c_int);

        pub fn luaL_loadfilex(
            self, file_name: *const c_char, mode: *const c_char
        ) -> c_int;

        pub fn luaL_loadbufferx(
            self,
            buffer: *const c_char, buffer_sz: usize,
            name: *const c_char,
            mode: *const c_char
        ) -> c_int;
        pub fn luaL_loadstring(self, code: *const c_char) -> c_int;

        pub fn luaL_len(self, idx: c_int) -> Integer;

        pub fn luaL_gsub(
            self,
            haystack: *const c_char,
            needle: *const c_char, replacement: *const c_char
        ) -> *const c_char;

        pub fn luaL_setfuncs(self, list: *const luaL_Reg, n_upvalues: c_int);
        pub fn luaL_getsubtable(
            self, idx: c_int, table_name: *const c_char
        ) -> c_int;

        pub fn luaL_traceback(
            self, of: *mut lua_State,
            message: *const c_char, level: c_int
        );

        pub fn luaL_requiref(
            self, module_name: *const c_char,
            open_fn: lua_CFunction, into_global: c_int
        );

        pub fn luaL_buffinit(self, buffer: *mut luaL_Buffer);
        pub fn luaL_buffinitsize(self, buffer: *mut luaL_Buffer, size: usize) -> *mut c_char;
    }
}

/// Equivalent to the `luaL_checkversion` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub unsafe fn luaL_checkversion(l: *mut lua_State) {
    unsafe { luaL_checkversion_(l, VERSION_NUM, NUM_SIZES) }
}

/// Equivalent to the `luaL_loadfile` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `file_name` must be a valid C string.
pub unsafe fn luaL_loadfile(l: *mut lua_State, file_name: *const c_char) -> c_int {
    unsafe { luaL_loadfilex(l, file_name, null()) }
}

/// Functionally equivalent to the `luaL_newlibtable` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `lib` has to be terminated with a sentinel pair - see [`luaL_Reg`].
pub unsafe fn luaL_newlibtable(l: *mut lua_State, lib: &[luaL_Reg]) {
    unsafe { lua_createtable(l, 0, (lib.len() - 1) as _) }
}

/// Functionally equivalent to the `luaL_newlib` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `lib` has to be terminated with a sentinel pair - see [`luaL_Reg`].
pub unsafe fn luaL_newlib(l: *mut lua_State, lib: &[luaL_Reg]) {
    unsafe {
        luaL_checkversion(l);
        luaL_newlibtable(l, lib);
        luaL_setfuncs(l, lib.as_ptr(), 0)
    }
}

// `luaL_argcheck` and `luaL_argexpected` omitted here because they're kind of
// useless. Maybe later.

/// Equivalent to the `luaL_checkstring` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub unsafe fn luaL_checkstring(l: *mut lua_State, arg: c_int) -> *const c_char {
    unsafe { luaL_checklstring(l, arg, null_mut()) }
}

/// Equivalent to the `luaL_optstring` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub unsafe fn luaL_optstring(
    l: *mut lua_State,
    arg: c_int,
    default: *const c_char,
) -> *const c_char {
    unsafe { luaL_optlstring(l, arg, default, null_mut()) }
}

/// Equivalent to the `luaL_typename` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub unsafe fn luaL_typename(l: *mut lua_State, idx: c_int) -> *const c_char {
    unsafe { lua_typename(l, lua_type(l, idx)) }
}

/// Equivalent to the `luaL_dofile` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `file_name` must be a valid C string.
pub unsafe fn luaL_dofile(l: *mut lua_State, file_name: *const c_char) -> bool {
    unsafe { luaL_loadfile(l, file_name) != 0 || lua_pcall(l, 0, MULT_RET, 0) != 0 }
}

/// Equivalent to the `luaL_dofile` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `code` must be a valid C string.
pub unsafe fn luaL_dostring(l: *mut lua_State, code: *const c_char) -> bool {
    unsafe { luaL_loadstring(l, code) != 0 || lua_pcall(l, 0, MULT_RET, 0) != 0 }
}

/// Equivalent to the `luaL_getmetatable` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state,
/// and `name` must be a valid C string.
pub unsafe fn luaL_getmetatable(l: *mut lua_State, name: *const c_char) -> c_int {
    unsafe { lua_getfield(l, REGISTRY_INDEX, name) }
}

// `luaL_opt` omitted here because it can be written out easily.

/// Equivalent to the `luaL_loadbuffer` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
/// `buffer` must be the size of `buffer_sz`,
/// and `name` must be a valid C string.
pub unsafe fn luaL_loadbuffer(
    l: *mut lua_State,
    buffer: *const c_char,
    buffer_sz: usize,
    name: *const c_char,
) -> c_int {
    unsafe { luaL_loadbufferx(l, buffer, buffer_sz, name, null()) }
}

// `luaL_intop` omitted here because it can be written out easily.

/// Equivalent to the `luaL_pushfail` C macro.
///
/// # Safety
/// `l` must be a valid pointer to a Lua state.
pub unsafe fn luaL_pushfail(l: *mut lua_State) {
    unsafe { lua_pushnil(l) }
}

/// Initial buffer size used by the buffer system, for [`luaL_Buffer`].
/// Also known as `LUAL_BUFFERSIZE`.
///
/// This cannot be changed for Lua that's already compiled.
// TODO: Does it make sense to ever change this?
pub const BUFFER_SIZE: usize = 16 * size_of::<*mut c_void>() * size_of::<Number>();

/// String buffer that allows code to build Lua strings piecemeal.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct luaL_Buffer {
    pub b: *mut c_char,
    pub size: usize,
    pub n: usize,
    pub L: *mut lua_State,
    pub init: luaL_Buffer_init,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)] // Can't be `transparent` because `_alignment` is not 1-aligned.
pub struct luaL_Buffer_init {
    pub _alignment: [MaxAlign; 0],
    pub b: [c_char; BUFFER_SIZE],
}
const _: () = assert!(
    size_of::<luaL_Buffer_init>() == size_of::<[c_char; BUFFER_SIZE]>(),
    "`luaL_Buffer_init` could not be properly aligned; this is a bug!",
);
