//! The part of Lua's C API this crate calls, with the few wrappers the rest of
//! the crate uses on a `lua_State`.
//!
//! Declarations follow `lua.h`, `lauxlib.h` and `lualib.h` as patched for
//! Barracuda (`patches/`), plus `c/require.c`.

#![allow(non_camel_case_types)]

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_long, c_uchar, c_ushort, c_void};
use core::marker::{PhantomData, PhantomPinned};
use core::mem::align_of;
use core::ops::{Deref, DerefMut};
use core::ptr::null_mut;
use core::slice::from_raw_parts;

pub type lua_Integer = i64;
pub type lua_Number = f64;
pub type lua_Unsigned = u64;
pub type lua_KContext = isize;

/// An opaque `lua_State`.
#[repr(C)]
pub struct lua_State {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

pub type lua_CFunction = unsafe extern "C-unwind" fn(state: *mut lua_State) -> c_int;
pub type lua_KFunction = unsafe extern "C-unwind" fn(
    state: *mut lua_State,
    status: c_int,
    context: lua_KContext,
) -> c_int;
pub type lua_Alloc = unsafe extern "C-unwind" fn(
    userdata: *mut c_void,
    allocation: *mut c_void,
    old_size: usize,
    new_size: usize,
) -> *mut c_void;
pub type lua_Hook = unsafe extern "C-unwind" fn(state: *mut lua_State, record: *mut lua_Debug);

/// `LUA_IDSIZE`.
const ID_SIZE: usize = 60;

#[repr(C)]
pub struct lua_Debug {
    pub event: c_int,
    pub name: *const c_char,
    pub namewhat: *const c_char,
    pub what: *const c_char,
    pub source: *const c_char,
    pub srclen: usize,
    pub currentline: c_int,
    pub linedefined: c_int,
    pub lastlinedefined: c_int,
    pub nups: c_uchar,
    pub nparams: c_uchar,
    pub isvararg: c_char,
    pub istailcall: c_char,
    pub ftransfer: c_ushort,
    pub ntransfer: c_ushort,
    pub short_src: [c_char; ID_SIZE],
    i_ci: *const c_void,
}

/// `LUA_MULTRET`.
pub const MULT_RET: c_int = -1;
/// `LUAI_MAXSTACK`.
const MAX_STACK: c_int = 1_000_000;
/// `LUA_REGISTRYINDEX`.
pub const REGISTRY_INDEX: c_int = -MAX_STACK - 1000;
/// `LUA_RIDX_GLOBALS`.
pub const REGISTRY_GLOBALS: lua_Integer = 2;
/// `LUA_EXTRASPACE`.
pub const DEFAULT_EXTRA_SPACE: usize = size_of::<*mut c_void>();
/// `LUA_LOADED_TABLE`.
pub const LOADED_TABLE: &CStr = c"_LOADED";
/// `LUA_PRELOAD_TABLE`.
pub const PRELOAD_TABLE: &CStr = c"_PRELOAD";
/// `LUA_NOREF`.
pub const NO_REF: c_int = -2;
/// `LUA_MASKCOUNT`.
pub const MASK_COUNT: c_int = 1 << 3;
/// `LUA_ERRMEM`.
pub const STATUS_MEMORY_ERROR: c_int = 4;

/// `LUAI_MAXALIGN`: the alignment of every block Lua allocates.
#[repr(C)]
union MaxAlign {
    number: lua_Number,
    pointer: *mut c_void,
    integer: lua_Integer,
    long: c_long,
}

pub const MAX_ALIGN: usize = align_of::<MaxAlign>();

/// Lua value types (`LUA_T*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    None,
    Nil,
    Boolean,
    LightUserdata,
    Number,
    String,
    Table,
    Function,
    Userdata,
    Thread,
}

impl Type {
    fn from_c(tag: c_int) -> Self {
        match tag {
            0 => Self::Nil,
            1 => Self::Boolean,
            2 => Self::LightUserdata,
            3 => Self::Number,
            4 => Self::String,
            5 => Self::Table,
            6 => Self::Function,
            7 => Self::Userdata,
            8 => Self::Thread,
            _ => Self::None,
        }
    }
}

unsafe extern "C-unwind" {
    pub fn lua_newstate(allocator: lua_Alloc, userdata: *mut c_void) -> *mut lua_State;
    pub fn lua_close(state: *mut lua_State);
    pub fn lua_newthread(state: *mut lua_State) -> *mut lua_State;
    pub fn lua_resetthread(state: *mut lua_State) -> c_int;

    pub fn lua_gettop(state: *mut lua_State) -> c_int;
    pub fn lua_settop(state: *mut lua_State, index: c_int);
    pub fn lua_pushvalue(state: *mut lua_State, index: c_int);
    pub fn lua_rotate(state: *mut lua_State, index: c_int, count: c_int);
    pub fn lua_xmove(from: *mut lua_State, to: *mut lua_State, count: c_int);

    pub fn lua_isinteger(state: *mut lua_State, index: c_int) -> c_int;
    pub fn lua_type(state: *mut lua_State, index: c_int) -> c_int;
    pub fn lua_tonumberx(state: *mut lua_State, index: c_int, is_number: *mut c_int) -> lua_Number;
    pub fn lua_tointegerx(
        state: *mut lua_State,
        index: c_int,
        is_number: *mut c_int,
    ) -> lua_Integer;
    pub fn lua_toboolean(state: *mut lua_State, index: c_int) -> c_int;
    pub fn lua_tolstring(state: *mut lua_State, index: c_int, length: *mut usize) -> *const c_char;
    pub fn lua_rawlen(state: *mut lua_State, index: c_int) -> lua_Unsigned;
    pub fn lua_touserdata(state: *mut lua_State, index: c_int) -> *mut c_void;

    pub fn lua_pushnil(state: *mut lua_State);
    pub fn lua_pushnumber(state: *mut lua_State, value: lua_Number);
    pub fn lua_pushinteger(state: *mut lua_State, value: lua_Integer);
    pub fn lua_pushlstring(
        state: *mut lua_State,
        text: *const c_char,
        length: usize,
    ) -> *const c_char;
    pub fn lua_pushcclosure(state: *mut lua_State, function: lua_CFunction, upvalues: c_int);
    pub fn lua_pushboolean(state: *mut lua_State, value: c_int);
    pub fn lua_pushlightuserdata(state: *mut lua_State, pointer: *mut c_void);

    pub fn lua_getglobal(state: *mut lua_State, name: *const c_char) -> c_int;
    pub fn lua_getfield(state: *mut lua_State, index: c_int, key: *const c_char) -> c_int;
    pub fn lua_rawget(state: *mut lua_State, index: c_int) -> c_int;
    pub fn lua_rawgeti(state: *mut lua_State, index: c_int, key: lua_Integer) -> c_int;
    pub fn lua_createtable(state: *mut lua_State, array: c_int, hash: c_int);
    pub fn lua_newuserdatauv(state: *mut lua_State, size: usize, values: c_int) -> *mut c_void;

    pub fn lua_setfield(state: *mut lua_State, index: c_int, key: *const c_char);
    pub fn lua_rawset(state: *mut lua_State, index: c_int);
    pub fn lua_rawseti(state: *mut lua_State, index: c_int, key: lua_Integer);
    pub fn lua_setupvalue(state: *mut lua_State, function: c_int, upvalue: c_int) -> *const c_char;

    pub fn lua_pcallk(
        state: *mut lua_State,
        arguments: c_int,
        results: c_int,
        handler: c_int,
        context: lua_KContext,
        continuation: Option<lua_KFunction>,
    ) -> c_int;
    pub fn lua_yieldk(
        state: *mut lua_State,
        results: c_int,
        context: lua_KContext,
        continuation: Option<lua_KFunction>,
    ) -> c_int;
    pub fn lua_resume(
        state: *mut lua_State,
        from: *mut lua_State,
        arguments: c_int,
        results: *mut c_int,
    ) -> c_int;
    pub fn lua_error(state: *mut lua_State) -> !;
    pub fn lua_sethook(state: *mut lua_State, hook: Option<lua_Hook>, mask: c_int, count: c_int);

    pub fn luaL_newmetatable(state: *mut lua_State, name: *const c_char) -> c_int;
    pub fn luaL_setmetatable(state: *mut lua_State, name: *const c_char);
    pub fn luaL_testudata(state: *mut lua_State, index: c_int, name: *const c_char) -> *mut c_void;
    pub fn luaL_ref(state: *mut lua_State, table: c_int) -> c_int;
    pub fn luaL_unref(state: *mut lua_State, table: c_int, reference: c_int);
    pub fn luaL_loadbufferx(
        state: *mut lua_State,
        buffer: *const c_char,
        size: usize,
        name: *const c_char,
        mode: *const c_char,
    ) -> c_int;
    pub fn luaL_requiref(
        state: *mut lua_State,
        name: *const c_char,
        open: lua_CFunction,
        global: c_int,
    );

    pub fn luaopen_base(state: *mut lua_State) -> c_int;
    pub fn luaopen_table(state: *mut lua_State) -> c_int;
    pub fn luaopen_string(state: *mut lua_State) -> c_int;
    pub fn luaopen_utf8(state: *mut lua_State) -> c_int;

    /// Installs the preload-only `require` (`c/require.c`).
    pub fn barracuda_lua_open_require(state: *mut lua_State);
}

/// `lua_upvalueindex`.
pub const fn lua_upvalueindex(index: c_int) -> c_int {
    REGISTRY_INDEX - index
}

/// `lua_pop`.
///
/// # Safety
///
/// `state` is a valid Lua state with at least `count` values on its stack.
pub unsafe fn lua_pop(state: *mut lua_State, count: c_int) {
    unsafe { lua_settop(state, -count - 1) }
}

/// `lua_remove`.
///
/// # Safety
///
/// `state` is a valid Lua state and `index` a valid stack index.
pub unsafe fn lua_remove(state: *mut lua_State, index: c_int) {
    unsafe {
        lua_rotate(state, index, -1);
        lua_pop(state, 1);
    }
}

/// `lua_getextraspace`.
///
/// # Safety
///
/// `state` is a valid Lua state and `size` is `LUA_EXTRASPACE`.
pub unsafe fn lua_getextraspace(state: *mut lua_State, size: usize) -> *mut c_void {
    unsafe { state.cast::<u8>().sub(size).cast::<c_void>() }
}

/// A Lua thread, always used behind a reference to its `lua_State`.
#[repr(transparent)]
pub struct Thread {
    raw: UnsafeCell<lua_State>,
}

impl Thread {
    /// # Safety
    ///
    /// `state` points to a valid Lua state for `'a`.
    pub unsafe fn from_ptr<'a>(state: *mut lua_State) -> &'a Self {
        unsafe { &*state.cast::<Self>() }
    }

    /// # Safety
    ///
    /// `state` points to a valid Lua state for `'a`, with no other reference
    /// to it.
    pub unsafe fn from_ptr_mut<'a>(state: *mut lua_State) -> &'a mut Self {
        unsafe { &mut *state.cast::<Self>() }
    }

    pub fn as_ptr(&self) -> *mut lua_State {
        self.raw.get()
    }

    pub fn top(&self) -> c_int {
        unsafe { lua_gettop(self.as_ptr()) }
    }

    pub fn type_of(&self, index: c_int) -> Type {
        Type::from_c(unsafe { lua_type(self.as_ptr(), index) })
    }

    pub fn is_integer(&self, index: c_int) -> bool {
        unsafe { lua_isinteger(self.as_ptr(), index) != 0 }
    }

    pub fn to_boolean(&self, index: c_int) -> bool {
        unsafe { lua_toboolean(self.as_ptr(), index) != 0 }
    }

    pub fn to_integer(&self, index: c_int) -> lua_Integer {
        unsafe { lua_tointegerx(self.as_ptr(), index, null_mut()) }
    }

    pub fn to_number(&self, index: c_int) -> lua_Number {
        unsafe { lua_tonumberx(self.as_ptr(), index, null_mut()) }
    }

    /// The bytes of the string at `index`, converting a number in place.
    pub fn to_string(&self, index: c_int) -> Option<&[u8]> {
        let mut length = 0;
        let text = unsafe { lua_tolstring(self.as_ptr(), index, &raw mut length) };
        (!text.is_null()).then(|| unsafe { from_raw_parts(text.cast::<u8>(), length) })
    }

    pub fn raw_length(&self, index: c_int) -> lua_Unsigned {
        unsafe { lua_rawlen(self.as_ptr(), index) }
    }

    pub fn push_nil(&self) {
        unsafe { lua_pushnil(self.as_ptr()) }
    }

    pub fn push_boolean(&self, value: bool) {
        unsafe { lua_pushboolean(self.as_ptr(), c_int::from(value)) }
    }

    pub fn push_integer(&self, value: lua_Integer) {
        unsafe { lua_pushinteger(self.as_ptr(), value) }
    }

    pub fn push_number(&self, value: lua_Number) {
        unsafe { lua_pushnumber(self.as_ptr(), value) }
    }

    pub fn push_string(&mut self, bytes: impl AsRef<[u8]>) {
        let bytes = bytes.as_ref();
        unsafe { lua_pushlstring(self.as_ptr(), bytes.as_ptr().cast(), bytes.len()) };
    }

    /// # Safety
    ///
    /// Lua may hand `pointer` back to code that dereferences it.
    pub unsafe fn push_light_userdata(&self, pointer: *mut c_void) {
        unsafe { lua_pushlightuserdata(self.as_ptr(), pointer) }
    }

    pub fn new_table(&mut self) {
        unsafe { lua_createtable(self.as_ptr(), 0, 0) }
    }

    /// Pushes the registry metatable `name`, creating it if needed; returns
    /// whether it was created.
    pub fn new_metatable(&mut self, name: &CStr) -> bool {
        unsafe { luaL_newmetatable(self.as_ptr(), name.as_ptr()) != 0 }
    }

    /// # Safety
    ///
    /// The returned block is uninitialized.
    pub unsafe fn new_userdata_raw(&mut self, size: usize, values: c_int) -> *mut c_void {
        unsafe { lua_newuserdatauv(self.as_ptr(), size, values) }
    }

    /// Pops the top value into a new reference in the table at `table`.
    pub fn create_ref(&mut self, table: c_int) -> c_int {
        unsafe { luaL_ref(self.as_ptr(), table) }
    }

    pub fn destroy_ref(&self, table: c_int, reference: c_int) {
        unsafe { luaL_unref(self.as_ptr(), table, reference) }
    }

    /// Raises the value on top of the stack as a Lua error.
    pub fn error(&self) -> ! {
        unsafe { lua_error(self.as_ptr()) }
    }

    /// # Safety
    ///
    /// Called from a C function, as its return.
    pub unsafe fn yield_with(&self, results: c_int) -> ! {
        unsafe {
            lua_yieldk(self.as_ptr(), results, 0, None);
            // Outside a hook, lua_yieldk does not return.
            core::hint::unreachable_unchecked()
        }
    }

    /// # Safety
    ///
    /// Called from a C function, as its return.
    pub unsafe fn yield_k_with(
        &self,
        results: c_int,
        continuation: lua_KFunction,
        context: lua_KContext,
    ) -> ! {
        unsafe {
            lua_yieldk(self.as_ptr(), results, context, Some(continuation));
            // Outside a hook, lua_yieldk does not return.
            core::hint::unreachable_unchecked()
        }
    }

    /// # Safety
    ///
    /// Called from a count or line hook, which then returns.
    pub unsafe fn yield_in_hook_with(&self, results: c_int) {
        unsafe { lua_yieldk(self.as_ptr(), results, 0, None) };
    }
}

/// A main Lua state, closed when dropped.
pub struct MainThread {
    thread: &'static mut Thread,
}

impl MainThread {
    /// Creates a state that allocates through `allocator`, or `None` when it
    /// cannot allocate the state itself.
    ///
    /// # Safety
    ///
    /// `userdata` stays valid for `allocator` until the state is dropped.
    pub unsafe fn new(allocator: lua_Alloc, userdata: *mut c_void) -> Option<Self> {
        let state = unsafe { lua_newstate(allocator, userdata) };
        (!state.is_null()).then(|| Self {
            thread: unsafe { Thread::from_ptr_mut(state) },
        })
    }
}

impl Drop for MainThread {
    fn drop(&mut self) {
        unsafe { lua_close(self.thread.as_ptr()) }
    }
}

impl Deref for MainThread {
    type Target = Thread;

    fn deref(&self) -> &Thread {
        self.thread
    }
}

impl DerefMut for MainThread {
    fn deref_mut(&mut self) -> &mut Thread {
        self.thread
    }
}
