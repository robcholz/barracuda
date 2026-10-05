use alloc::{boxed::Box, ffi::CString, rc::Rc, vec::Vec};
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::{Cell, RefCell},
    ffi::{c_int, c_void},
    future::Future,
    pin::Pin,
    ptr::{NonNull, null_mut},
    task::{Context, Poll, Waker},
};

use lunka::Thread;
use lunka::cdef::auxlib::{LOADED_TABLE, PRELOAD_TABLE, luaL_loadbufferx, luaL_ref};
use lunka::cdef::stdlibs::{
    luaopen_base, luaopen_math, luaopen_package, luaopen_string, luaopen_table, luaopen_utf8,
};
use lunka::cdef::{
    DEFAULT_EXTRA_SPACE, EventMask, MAX_ALIGN, REGISTRY_GLOBALS, REGISTRY_INDEX, Status,
    lua_CFunction, lua_Debug, lua_KContext, lua_State, lua_createtable, lua_getextraspace,
    lua_getfield, lua_getglobal, lua_gettop, lua_newthread, lua_pop, lua_pushcclosure,
    lua_pushlightuserdata, lua_pushlstring, lua_pushnil, lua_pushvalue, lua_rawgeti, lua_rawseti,
    lua_remove, lua_resetthread, lua_resume, lua_setfield, lua_sethook, lua_settop, lua_setupvalue,
    lua_tolstring, lua_touserdata, lua_xmove,
};

use crate::{
    Context as LuaContext, Error, ErrorKind, FromLuaMulti, IntoLua, IntoLuaMulti, LuaReturn,
    RegistryKey, Result,
};

pub(crate) type ReturnPusher = Box<dyn FnOnce(&mut Thread) -> Result<c_int> + 'static>;
pub(crate) type NativeFuture = Pin<Box<dyn Future<Output = ReturnPusher> + 'static>>;
pub(crate) type SyncCallback = Box<dyn Fn(&mut Thread, c_int) -> Result<c_int> + 'static>;
pub(crate) type AsyncCallback = Box<dyn Fn(&mut Thread, c_int) -> Result<NativeFuture> + 'static>;

pub(crate) enum Callback {
    Sync(SyncCallback),
    Async(AsyncCallback),
}

type InstructionHook = Box<dyn FnMut() + 'static>;

pub(crate) struct Task {
    pub(crate) future: NativeFuture,
}

static ASYNC_MARKER: u8 = 0xA5;
static ERROR_MARKER: u8 = 0xE1;

const ENVIRONMENT_GLOBALS: [&core::ffi::CStr; 23] = [
    c"assert",
    c"error",
    c"getmetatable",
    c"ipairs",
    c"next",
    c"pairs",
    c"pcall",
    c"rawequal",
    c"rawget",
    c"rawlen",
    c"rawset",
    c"require",
    c"select",
    c"setmetatable",
    c"tonumber",
    c"tostring",
    c"type",
    c"xpcall",
    c"string",
    c"table",
    c"math",
    c"utf8",
    c"_VERSION",
];

const SANITIZE_STANDARD_LIBRARIES: &str = r#"
local native_randomseed = math.randomseed
native_randomseed(0, 0)
function math.randomseed(first, second)
    if first == nil then
        return native_randomseed(0, 0)
    end
    return native_randomseed(first, second)
end
"#;

pub struct Lua {
    raw: lunka::Lua,
    state: Rc<State>,
    environment: c_int,
}

unsafe fn retain_preload_searcher(state: *mut lua_State) {
    unsafe {
        lua_getglobal(state, c"package".as_ptr());
        lua_getfield(state, -1, c"searchers".as_ptr());
        lua_createtable(state, 1, 0);
        lua_rawgeti(state, -2, 1);
        lua_rawseti(state, -2, 1);
        lua_setfield(state, -3, c"searchers".as_ptr());
        lua_pop(state, 2);
    }
}

unsafe fn create_environment(state: *mut lua_State) -> c_int {
    unsafe {
        lua_createtable(state, 0, 14);
        for name in ENVIRONMENT_GLOBALS {
            lua_getglobal(state, name.as_ptr());
            lua_setfield(state, -2, name.as_ptr());
        }
        lua_pushvalue(state, -1);
        lua_setfield(state, -2, c"_G".as_ptr());
        luaL_ref(state, REGISTRY_INDEX)
    }
}

unsafe fn discard_bootstrap_environment(state: *mut lua_State, environment: c_int) {
    unsafe {
        lua_getfield(state, REGISTRY_INDEX, LOADED_TABLE.as_ptr());
        lua_pushnil(state);
        lua_setfield(state, -2, c"_G".as_ptr());
        lua_pushnil(state);
        lua_setfield(state, -2, c"package".as_ptr());
        lua_pop(state, 1);

        lua_rawgeti(state, REGISTRY_INDEX, environment.into());
        lua_rawseti(state, REGISTRY_INDEX, REGISTRY_GLOBALS);
    }
}

pub(crate) struct State {
    main: Cell<*mut lua_State>,
    callbacks: RefCell<Vec<Rc<Callback>>>,
    instruction_hook: RefCell<Option<InstructionHook>>,
    instruction_hook_interval: Cell<c_int>,
    hook_yielded: Cell<bool>,
}

impl State {
    pub(crate) fn main(&self) -> Result<*mut lua_State> {
        let main = self.main.get();
        if main.is_null() {
            Err(Error::new(ErrorKind::Runtime, "Lua state has been dropped"))
        } else {
            Ok(main)
        }
    }

    pub(crate) fn store_callback(&self, callback: Callback) -> usize {
        let mut callbacks = self.callbacks.borrow_mut();
        let index = callbacks.len();
        callbacks.push(Rc::new(callback));
        index
    }

    pub(crate) fn callback_count(&self) -> usize {
        self.callbacks.borrow().len()
    }

    pub(crate) fn truncate_callbacks(&self, count: usize) {
        self.callbacks.borrow_mut().truncate(count);
    }

    pub(crate) fn push_callback(&self, lua: &mut Thread, index: usize) -> Result<()> {
        let callbacks = self.callbacks.borrow();
        let callback = callbacks
            .get(index)
            .ok_or_else(|| Error::new(ErrorKind::Runtime, "native callback index is invalid"))?;
        let callback_ptr = Rc::as_ptr(callback).cast_mut().cast::<c_void>();
        let dispatcher = match callback.as_ref() {
            Callback::Sync(_) => sync_dispatch,
            Callback::Async(_) => async_dispatch,
        };
        unsafe {
            lua_pushlightuserdata(lua.as_ptr(), callback_ptr);
            lua_pushcclosure(lua.as_ptr(), dispatcher as lua_CFunction, 1);
        }
        Ok(())
    }

    fn install_instruction_hook(&self, coroutine: *mut lua_State) {
        let interval = self.instruction_hook_interval.get();
        let enabled = self.instruction_hook.borrow().is_some() && interval > 0;
        let hook = enabled.then_some(instruction_hook_dispatch as _);
        let mask = if enabled { EventMask::COUNT.0 } else { 0 };
        unsafe { lua_sethook(coroutine, hook, mask, interval) };
    }
}

unsafe extern "C-unwind" fn rust_allocator<A>(
    userdata: *mut c_void,
    allocation: *mut c_void,
    old_size: usize,
    new_size: usize,
) -> *mut c_void
where
    A: GlobalAlloc,
{
    let allocator = unsafe { &*userdata.cast::<A>() };
    if allocation.is_null() {
        if new_size == 0 {
            return null_mut();
        }
        let Ok(layout) = Layout::from_size_align(new_size, MAX_ALIGN) else {
            return null_mut();
        };
        return unsafe { allocator.alloc(layout).cast::<c_void>() };
    }
    let Ok(old_layout) = Layout::from_size_align(old_size, MAX_ALIGN) else {
        return null_mut();
    };
    if new_size == 0 {
        unsafe { allocator.dealloc(allocation.cast::<u8>(), old_layout) };
        return null_mut();
    }
    unsafe {
        allocator
            .realloc(allocation.cast::<u8>(), old_layout, new_size)
            .cast::<c_void>()
    }
}

impl Lua {
    pub fn new() -> Result<Self> {
        let raw = lunka::Lua::try_new()
            .ok_or_else(|| Error::new(ErrorKind::Create, "failed to create Lua state"))?;
        Self::initialize(raw)
    }

    /// Creates a sandboxed Lua state using an externally owned Rust allocator.
    ///
    /// # Safety
    ///
    /// `allocator` must remain valid until the returned Lua state is dropped.
    /// Its backing memory must also remain valid, and the allocator must obey
    /// [`GlobalAlloc`]'s allocation contract.
    pub unsafe fn new_with_allocator<A>(allocator: *const A) -> Result<Self>
    where
        A: GlobalAlloc,
    {
        if allocator.is_null() {
            return Err(Error::new(ErrorKind::Create, "Lua allocator is null"));
        }
        let raw = unsafe {
            lunka::Lua::try_new_with_alloc_fn(
                rust_allocator::<A>,
                allocator.cast_mut().cast::<c_void>(),
            )
        }
        .ok_or_else(|| Error::new(ErrorKind::Memory, "external Lua allocator is exhausted"))?;
        Self::initialize(raw)
    }

    fn initialize(mut raw: lunka::Lua) -> Result<Self> {
        let state = raw.as_ptr();
        unsafe {
            lunka::cdef::auxlib::luaL_requiref(state, c"_G".as_ptr(), luaopen_base, 1);
            lua_pop(state, 1);
            lunka::cdef::auxlib::luaL_requiref(state, c"package".as_ptr(), luaopen_package, 1);
            lua_pop(state, 1);
            lunka::cdef::auxlib::luaL_requiref(state, c"string".as_ptr(), luaopen_string, 1);
            lua_pop(state, 1);
            lunka::cdef::auxlib::luaL_requiref(state, c"table".as_ptr(), luaopen_table, 1);
            lua_pop(state, 1);
            lunka::cdef::auxlib::luaL_requiref(state, c"math".as_ptr(), luaopen_math, 1);
            lua_pop(state, 1);
            lunka::cdef::auxlib::luaL_requiref(state, c"utf8".as_ptr(), luaopen_utf8, 1);
            lua_pop(state, 1);
        }
        let environment = unsafe {
            retain_preload_searcher(state);
            create_environment(state)
        };
        unsafe { discard_bootstrap_environment(state, environment) };
        let shared = Rc::new(State {
            main: Cell::new(state),
            callbacks: RefCell::new(Vec::new()),
            instruction_hook: RefCell::new(None),
            instruction_hook_interval: Cell::new(0),
            hook_yielded: Cell::new(false),
        });
        unsafe {
            let slot = lua_getextraspace(state, DEFAULT_EXTRA_SPACE).cast::<*const State>();
            slot.write(Rc::as_ptr(&shared));
        }
        let mut lua = Self {
            raw,
            state: shared,
            environment,
        };
        lua.load(SANITIZE_STANDARD_LIBRARIES).exec()?;
        Ok(lua)
    }

    /// Installs a count hook called after every `instruction_interval` Lua instructions.
    ///
    /// The hook callback runs immediately before Lua yields from its current resume. It can
    /// notify an external scheduler, while scheduling policy remains outside this wrapper.
    /// The hook is internal and does not expose Lua's `debug` library to scripts.
    pub fn set_instruction_hook<F>(&mut self, instruction_interval: u32, hook: F) -> Result<()>
    where
        F: FnMut() + 'static,
    {
        let interval = c_int::try_from(instruction_interval).map_err(|_error| {
            Error::new(
                ErrorKind::Conversion,
                "instruction hook interval exceeds the Lua count range",
            )
        })?;
        if interval == 0 {
            return Err(Error::new(
                ErrorKind::Conversion,
                "instruction hook interval must be greater than zero",
            ));
        }
        self.state.instruction_hook.replace(Some(Box::new(hook)));
        self.state.instruction_hook_interval.set(interval);
        Ok(())
    }

    pub fn register<A, R, F>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> LuaReturn<R> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.store_sync(function);
        self.set_environment_callback(&name, index)
    }

    pub fn register_async<A, R, F, Fut>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.store_async(function);
        self.set_environment_callback(&name, index)
    }

    pub fn register_with<A, R, F>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> LuaReturn<R> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.store_sync_with(function);
        self.set_environment_callback(&name, index)
    }

    pub fn register_async_with<A, R, F, Fut>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.store_async_with(function);
        self.set_environment_callback(&name, index)
    }

    pub fn set<V: IntoLua>(&mut self, name: &str, value: V) -> Result<()> {
        let name = lua_name(name)?;
        let initial_top = self.raw.top();
        self.push_environment();
        let result = value.push_to_lua(&mut self.raw);
        if result.is_ok() {
            unsafe { lua_setfield(self.raw.as_ptr(), -2, name.as_ptr()) };
        }
        unsafe { lua_settop(self.raw.as_ptr(), initial_top) };
        result
    }

    pub fn register_lib<F>(&mut self, name: &str, configure: F) -> Result<()>
    where
        F: FnOnce(&mut Library<'_>) -> Result<()>,
    {
        let name = lua_name(name)?;
        let main_top = self.raw.top();
        let callback_count = self.state.callbacks.borrow().len();
        self.raw.managed().new_table();
        let table_index = self.raw.top();
        let result = configure(&mut Library {
            lua: self,
            table_index,
        });
        if let Err(error) = result {
            unsafe { lua_settop(self.raw.as_ptr(), main_top) };
            self.state.callbacks.borrow_mut().truncate(callback_count);
            return Err(error);
        }

        let state = self.raw.as_ptr();
        unsafe {
            lua_getfield(state, REGISTRY_INDEX, PRELOAD_TABLE.as_ptr());
            lua_pushvalue(state, table_index);
            lua_pushcclosure(state, preload_loader as lua_CFunction, 1);
            lua_setfield(state, -2, name.as_ptr());
            lua_settop(state, main_top);
        }
        Ok(())
    }

    /// Extends a table-valued library that has already been loaded by `require`.
    ///
    /// This is intended for capability packages that add standard-shaped APIs
    /// to a VM-owned library without replacing its existing state.
    pub fn extend_loaded_lib<F>(&mut self, name: &str, configure: F) -> Result<()>
    where
        F: FnOnce(&mut Library<'_>) -> Result<()>,
    {
        let name = lua_name(name)?;
        let main_top = self.raw.top();
        let callback_count = self.state.callback_count();
        unsafe {
            lua_getfield(self.raw.as_ptr(), REGISTRY_INDEX, LOADED_TABLE.as_ptr());
            lua_getfield(self.raw.as_ptr(), -1, name.as_ptr());
        }
        if self.raw.type_of(-1) != lunka::cdef::Type::Table {
            unsafe { lua_settop(self.raw.as_ptr(), main_top) };
            return Err(Error::runtime(alloc::format!(
                "Lua library `{}` is not loaded as a table",
                name.to_string_lossy()
            )));
        }
        let table_index = self.raw.top();
        let result = configure(&mut Library {
            lua: self,
            table_index,
        });
        if result.is_err() {
            self.state.truncate_callbacks(callback_count);
        }
        unsafe { lua_settop(self.raw.as_ptr(), main_top) };
        result
    }

    pub fn load<'lua, 'code>(&'lua mut self, code: &'code str) -> Chunk<'lua, 'code> {
        Chunk {
            lua: self,
            code: code.as_bytes(),
        }
    }

    pub fn create_registry_value<V: IntoLua>(&mut self, value: V) -> Result<RegistryKey> {
        LuaContext::new(&mut self.raw).create_registry_value(value)
    }

    pub fn registry_value<V: crate::FromLua>(&mut self, key: &RegistryKey) -> Result<V> {
        LuaContext::new(&mut self.raw).registry_value(key)
    }

    pub fn remove_registry_value(&mut self, key: RegistryKey) -> Result<()> {
        LuaContext::new(&mut self.raw).remove_registry_value(key)
    }

    fn store_sync<A, R, F>(&mut self, function: F) -> usize
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count| {
            let arguments = A::from_lua_multi(lua, count)?;
            push_lua_return(lua, function(arguments))
        };
        self.store_callback(Callback::Sync(Box::new(callback)))
    }

    fn store_async<A, R, F, Fut>(&mut self, function: F) -> usize
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let callback = move |lua: &mut Thread, count| {
            let arguments = A::from_lua_multi(lua, count)?;
            let future = function(arguments);
            let future: NativeFuture = Box::pin(async move {
                let value = future.await;
                let pusher: ReturnPusher = Box::new(move |lua| push_lua_return(lua, value));
                pusher
            });
            Ok(future)
        };
        self.store_callback(Callback::Async(Box::new(callback)))
    }

    fn store_sync_with<A, R, F>(&mut self, function: F) -> usize
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count| {
            let arguments = A::from_lua_multi(lua, count)?;
            let mut context = LuaContext::new(lua);
            let value = function(&mut context, arguments);
            push_lua_return(context.thread(), value)
        };
        self.store_callback(Callback::Sync(Box::new(callback)))
    }

    fn store_async_with<A, R, F, Fut>(&mut self, function: F) -> usize
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let callback = move |lua: &mut Thread, count| {
            let arguments = A::from_lua_multi(lua, count)?;
            let mut context = LuaContext::new(lua);
            let future = function(&mut context, arguments);
            let future: NativeFuture = Box::pin(async move {
                let value = future.await;
                let pusher: ReturnPusher = Box::new(move |lua| push_lua_return(lua, value));
                pusher
            });
            Ok(future)
        };
        self.store_callback(Callback::Async(Box::new(callback)))
    }

    fn store_callback(&mut self, callback: Callback) -> usize {
        self.state.store_callback(callback)
    }

    fn push_callback(&mut self, index: usize) -> Result<()> {
        self.state.push_callback(&mut self.raw, index)
    }

    fn push_environment(&mut self) {
        unsafe {
            lua_rawgeti(self.raw.as_ptr(), REGISTRY_INDEX, self.environment.into());
        }
    }

    fn set_environment_callback(&mut self, name: &CString, index: usize) -> Result<()> {
        let initial_top = self.raw.top();
        self.push_environment();
        let result = self.push_callback(index);
        if result.is_ok() {
            unsafe { lua_setfield(self.raw.as_ptr(), -2, name.as_ptr()) };
        } else {
            self.state.truncate_callbacks(index);
        }
        unsafe { lua_settop(self.raw.as_ptr(), initial_top) };
        result
    }

    fn start<R: FromLuaMulti>(&mut self, code: &[u8]) -> Execution<'_, R> {
        let state = ExecutionState::new(self, code, R::from_lua_multi);
        Execution { lua: self, state }
    }

    fn start_ignoring(&mut self, code: &[u8]) -> Execution<'_, ()> {
        let state = ExecutionState::new(self, code, ignore_lua_results);
        Execution { lua: self, state }
    }

    pub(crate) fn start_owned_ignoring(&mut self, code: &[u8]) -> ExecutionState<()> {
        ExecutionState::new(self, code, ignore_lua_results)
    }
}

impl Drop for Lua {
    fn drop(&mut self) {
        self.state.main.set(null_mut());
    }
}

pub struct Library<'lua> {
    lua: &'lua mut Lua,
    table_index: c_int,
}

impl Library<'_> {
    pub fn set<V: IntoLua>(&mut self, name: &str, value: V) -> Result<()> {
        let name = lua_name(name)?;
        value.push_to_lua(&mut self.lua.raw)?;
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }

    pub fn table<F>(&mut self, name: &str, configure: F) -> Result<()>
    where
        F: FnOnce(&mut Library<'_>) -> Result<()>,
    {
        let name = lua_name(name)?;
        let initial_top = self.lua.raw.top();
        let callback_count = self.lua.state.callbacks.borrow().len();
        self.lua.raw.managed().new_table();
        let table_index = self.lua.raw.top();
        let result = configure(&mut Library {
            lua: self.lua,
            table_index,
        });
        if let Err(error) = result {
            unsafe { lua_settop(self.lua.raw.as_ptr(), initial_top) };
            self.lua
                .state
                .callbacks
                .borrow_mut()
                .truncate(callback_count);
            return Err(error);
        }
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }

    pub fn register<A, R, F>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> LuaReturn<R> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.lua.store_sync(function);
        self.lua.push_callback(index)?;
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }

    pub fn register_async<A, R, F, Fut>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.lua.store_async(function);
        self.lua.push_callback(index)?;
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }

    pub fn register_with<A, R, F>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> LuaReturn<R> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.lua.store_sync_with(function);
        self.lua.push_callback(index)?;
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }

    pub fn register_async_with<A, R, F, Fut>(&mut self, name: &str, function: F) -> Result<()>
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut LuaContext<'_>, A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let name = lua_name(name)?;
        let index = self.lua.store_async_with(function);
        self.lua.push_callback(index)?;
        unsafe { lua_setfield(self.lua.raw.as_ptr(), self.table_index, name.as_ptr()) };
        Ok(())
    }
}

pub struct Chunk<'lua, 'code> {
    lua: &'lua mut Lua,
    code: &'code [u8],
}

impl<'lua> Chunk<'lua, '_> {
    pub fn eval<R: FromLuaMulti>(self) -> Result<R> {
        let mut execution = self.lua.start::<R>(self.code);
        let mut execution = Pin::new(&mut execution);
        match Future::poll(execution.as_mut(), &mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(Error::new(
                ErrorKind::UnexpectedYield,
                "native async function requires eval_async",
            )),
        }
    }

    pub fn eval_async<R: FromLuaMulti>(self) -> Execution<'lua, R> {
        self.lua.start(self.code)
    }

    pub fn exec(self) -> Result<()> {
        let mut execution = self.lua.start_ignoring(self.code);
        let mut execution = Pin::new(&mut execution);
        match Future::poll(execution.as_mut(), &mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(Error::new(
                ErrorKind::UnexpectedYield,
                "native async function requires exec_async",
            )),
        }
    }

    pub fn exec_async(self) -> Execution<'lua, ()> {
        self.lua.start_ignoring(self.code)
    }
}

fn ignore_lua_results(_lua: &mut Thread, _count: c_int) -> Result<()> {
    Ok(())
}

pub struct Execution<'lua, R> {
    lua: &'lua mut Lua,
    state: ExecutionState<R>,
}

pub(crate) struct ExecutionState<R> {
    coroutine: *mut lua_State,
    main_top: c_int,
    resume_args: c_int,
    pending: Option<NonNull<Task>>,
    init_error: Option<Error>,
    finished: bool,
    convert: fn(&mut Thread, c_int) -> Result<R>,
}

impl<R> ExecutionState<R> {
    fn new(lua: &mut Lua, code: &[u8], convert: fn(&mut Thread, c_int) -> Result<R>) -> Self {
        let state = lua.raw.as_ptr();
        let main_top = unsafe { lua_gettop(state) };
        let load_status = unsafe {
            luaL_loadbufferx(
                state,
                code.as_ptr().cast(),
                code.len(),
                c"=chunk".as_ptr(),
                c"t".as_ptr(),
            )
        };
        if load_status != 0 {
            let kind = if load_status == Status::MemoryError as c_int {
                ErrorKind::Memory
            } else {
                ErrorKind::Load
            };
            let error = unsafe { take_lua_error(state, kind) };
            unsafe { lua_settop(state, main_top) };
            return Self::failed(main_top, error, convert);
        }

        lua.push_environment();
        if unsafe { lua_setupvalue(state, -2, 1) }.is_null() {
            unsafe { lua_pop(state, 1) };
        }

        let coroutine = unsafe { lua_newthread(state) };
        unsafe {
            lua_pushvalue(state, -2);
            lua_xmove(state, coroutine, 1);
            lua_remove(state, -2);
        }
        lua.state.install_instruction_hook(coroutine);
        Self {
            coroutine,
            main_top,
            resume_args: 0,
            pending: None,
            init_error: None,
            finished: false,
            convert,
        }
    }

    fn failed(main_top: c_int, error: Error, convert: fn(&mut Thread, c_int) -> Result<R>) -> Self {
        Self {
            coroutine: null_mut(),
            main_top,
            resume_args: 0,
            pending: None,
            init_error: Some(error),
            finished: false,
            convert,
        }
    }

    pub(crate) fn cleanup(&mut self, lua: &mut Lua) {
        if self.finished {
            return;
        }
        if let Some(task) = self.pending.take() {
            unsafe { drop(Box::from_raw(task.as_ptr())) };
        }
        if !self.coroutine.is_null() {
            unsafe {
                let _ = lua_resetthread(self.coroutine);
            }
        }
        unsafe { lua_settop(lua.raw.as_ptr(), self.main_top) };
        self.finished = true;
    }

    pub(crate) fn poll(&mut self, lua: &mut Lua, context: &mut Context<'_>) -> Poll<Result<R>> {
        let this = self;
        if let Some(error) = this.init_error.take() {
            this.cleanup(lua);
            return Poll::Ready(Err(error));
        }
        if this.finished {
            return Poll::Ready(Err(Error::new(
                ErrorKind::Runtime,
                "execution polled after completion",
            )));
        }

        loop {
            if let Some(mut task_ptr) = this.pending {
                let task = unsafe { task_ptr.as_mut() };
                match task.future.as_mut().poll(context) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(push) => {
                        this.pending = None;
                        unsafe { drop(Box::from_raw(task_ptr.as_ptr())) };
                        unsafe { lua_settop(this.coroutine, 0) };
                        this.resume_args = {
                            let lua = unsafe { Thread::from_ptr_mut(this.coroutine) };
                            match push(lua) {
                                Ok(count) => count,
                                Err(error) => {
                                    unsafe { push_native_error(this.coroutine, &error) };
                                    2
                                }
                            }
                        };
                    }
                }
            }

            let mut result_count = 0;
            let status = unsafe {
                lua_resume(
                    this.coroutine,
                    null_mut(),
                    this.resume_args,
                    &mut result_count,
                )
            };
            this.resume_args = 0;
            match status {
                0 => {
                    let coroutine = unsafe { Thread::from_ptr_mut(this.coroutine) };
                    let output = (this.convert)(coroutine, result_count);
                    this.cleanup(lua);
                    return Poll::Ready(output);
                }
                1 => {
                    if lua.state.hook_yielded.replace(false) {
                        if result_count != 0 {
                            this.cleanup(lua);
                            return Poll::Ready(Err(Error::new(
                                ErrorKind::UnexpectedYield,
                                "instruction hook yielded Lua values",
                            )));
                        }
                        context.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    let task = unsafe { take_async_yield(this.coroutine, result_count) };
                    match task {
                        Ok(task) => this.pending = Some(task),
                        Err(error) => {
                            this.cleanup(lua);
                            return Poll::Ready(Err(error));
                        }
                    }
                }
                _ => {
                    let kind = if status == Status::MemoryError as c_int {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Runtime
                    };
                    let error = unsafe { take_lua_error(this.coroutine, kind) };
                    this.cleanup(lua);
                    return Poll::Ready(Err(error));
                }
            }
        }
    }
}

impl<R: FromLuaMulti> Future for Execution<'_, R> {
    type Output = Result<R>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.state.poll(this.lua, context)
    }
}

impl<R> Drop for Execution<'_, R> {
    fn drop(&mut self) {
        self.state.cleanup(self.lua);
    }
}

pub(crate) fn push_lua_return<R: IntoLuaMulti>(
    lua: &mut Thread,
    value: LuaReturn<R>,
) -> Result<c_int> {
    match value {
        None => Ok(0),
        Some(Ok(value)) => value.push_to_lua_multi(lua),
        Some(Err(error)) => {
            lua.push_nil();
            lua.managed().push_string(error.message().as_bytes());
            Ok(2)
        }
    }
}

unsafe extern "C-unwind" fn preload_loader(state: *mut lua_State) -> c_int {
    unsafe { lua_pushvalue(state, lunka::cdef::lua_upvalueindex(1)) };
    1
}

unsafe extern "C-unwind" fn instruction_hook_dispatch(
    state: *mut lua_State,
    _activation_record: *mut lua_Debug,
) {
    let lua = unsafe { Thread::from_ptr_mut(state) };
    let shared = match state_from_thread(lua) {
        Ok(shared) => shared,
        Err(error) => unsafe { raise_lua_error(state, error) },
    };
    {
        let mut hook = match shared.instruction_hook.try_borrow_mut() {
            Ok(hook) => hook,
            Err(_error) => unsafe {
                raise_lua_error(state, Error::runtime("instruction hook was re-entered"))
            },
        };
        if let Some(hook) = hook.as_mut() {
            hook();
        }
    }
    shared.hook_yielded.set(true);
    unsafe { lua.yield_in_hook_with(0) };
}

unsafe extern "C-unwind" fn sync_dispatch(state: *mut lua_State) -> c_int {
    let callback = unsafe { callback(state) };
    let lua = unsafe { Thread::from_ptr_mut(state) };
    let result = match callback {
        Some(Callback::Sync(call)) => call(lua, lua.top()),
        _ => Err(Error::runtime("invalid native sync function")),
    };
    match result {
        Ok(count) => count,
        Err(error) => unsafe { raise_lua_error(state, error) },
    }
}

unsafe extern "C-unwind" fn async_dispatch(state: *mut lua_State) -> c_int {
    let mut context = 0;
    let result_count = unsafe { prepare_async(state, &mut context) };
    let thread = unsafe { Thread::from_ptr(state) };
    if result_count < 0 {
        thread.error();
    }
    unsafe { thread.yield_k_with(result_count, async_continuation, context) }
}

unsafe extern "C-unwind" fn async_continuation(
    state: *mut lua_State,
    _status: c_int,
    _context: lua_KContext,
) -> c_int {
    let result_count = unsafe { finish_async(state) };
    if result_count < 0 {
        unsafe { Thread::from_ptr(state) }.error();
    }
    result_count
}

unsafe fn callback(state: *mut lua_State) -> Option<&'static Callback> {
    let pointer =
        unsafe { lua_touserdata(state, lunka::cdef::lua_upvalueindex(1)) as *const Callback };
    unsafe { pointer.as_ref() }
}

unsafe fn prepare_async(state: *mut lua_State, context: *mut lua_KContext) -> c_int {
    if context.is_null() {
        unsafe { push_error_bytes(state, b"invalid native async context") };
        return -1;
    }
    let callback = unsafe { callback(state) };
    let lua = unsafe { Thread::from_ptr_mut(state) };
    let future = match callback {
        Some(Callback::Async(call)) => match call(lua, lua.top()) {
            Ok(future) => future,
            Err(error) => {
                unsafe { push_error_bytes(state, error.message().as_bytes()) };
                return -1;
            }
        },
        _ => {
            unsafe { push_error_bytes(state, b"invalid native async function") };
            return -1;
        }
    };
    let task_ptr = Box::into_raw(Box::new(Task { future }));
    unsafe {
        *context = task_ptr as lua_KContext;
        lua_pushlightuserdata(state, (&raw const ASYNC_MARKER).cast_mut().cast());
        lua_pushlightuserdata(state, task_ptr.cast());
    }
    2
}

unsafe fn finish_async(state: *mut lua_State) -> c_int {
    let top = unsafe { lua_gettop(state) };
    if top == 2
        && unsafe { lua_touserdata(state, 1) } == (&raw const ERROR_MARKER).cast_mut().cast()
    {
        unsafe { lua_remove(state, 1) };
        -1
    } else {
        top
    }
}

pub(crate) unsafe fn take_async_yield(
    state: *mut lua_State,
    result_count: c_int,
) -> Result<NonNull<Task>> {
    if result_count != 2 {
        return Err(Error::new(
            ErrorKind::UnexpectedYield,
            "unexpected Lua yield",
        ));
    }
    let top = unsafe { lua_gettop(state) };
    if top < 2
        || unsafe { lua_touserdata(state, top - 1) } != (&raw const ASYNC_MARKER).cast_mut().cast()
    {
        return Err(Error::new(
            ErrorKind::UnexpectedYield,
            "unexpected Lua yield",
        ));
    }
    let task = unsafe { lua_touserdata(state, top) as *mut Task };
    NonNull::new(task).ok_or_else(|| Error::new(ErrorKind::Runtime, "native async task is null"))
}

unsafe fn raise_lua_error(state: *mut lua_State, error: Error) -> ! {
    unsafe { push_error_bytes(state, error.message().as_bytes()) };
    drop(error);
    unsafe { Thread::from_ptr(state) }.error()
}

pub(crate) unsafe fn push_native_error(state: *mut lua_State, error: &Error) {
    unsafe {
        lua_pushlightuserdata(state, (&raw const ERROR_MARKER).cast_mut().cast());
        push_error_bytes(state, error.message().as_bytes());
    }
}

unsafe fn push_error_bytes(state: *mut lua_State, bytes: &[u8]) {
    unsafe {
        lua_pushlstring(state, bytes.as_ptr().cast(), bytes.len());
    }
}

pub(crate) unsafe fn take_lua_error(state: *mut lua_State, kind: ErrorKind) -> Error {
    let top = unsafe { lua_gettop(state) };
    let bytes = if top > 0 {
        let mut length = 0;
        let pointer = unsafe { lua_tolstring(state, -1, &mut length) };
        if pointer.is_null() {
            b"unknown Lua error".to_vec()
        } else {
            unsafe { core::slice::from_raw_parts(pointer.cast(), length) }.to_vec()
        }
    } else {
        b"unknown Lua error".to_vec()
    };
    Error::from_lua_bytes(kind, bytes)
}

pub(crate) fn state_from_thread(lua: &mut Thread) -> Result<Rc<State>> {
    let pointer = unsafe {
        let slot = lua_getextraspace(lua.as_ptr(), DEFAULT_EXTRA_SPACE).cast::<*const State>();
        slot.read()
    };
    if pointer.is_null() {
        return Err(Error::new(
            ErrorKind::Runtime,
            "Lua state is not owned by barracuda-lua",
        ));
    }
    unsafe {
        Rc::increment_strong_count(pointer);
        Ok(Rc::from_raw(pointer))
    }
}

fn lua_name(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| Error::new(ErrorKind::Conversion, "Lua name contains NUL"))
}

#[cfg(test)]
mod tests {
    use core::{
        ffi::c_int,
        future::Future,
        pin::Pin,
        ptr::null,
        task::{Context, Poll, Waker},
    };

    use lunka::{
        Thread,
        cdef::{
            DEFAULT_EXTRA_SPACE, lua_CFunction, lua_State, lua_getextraspace, lua_pop,
            lua_pushcclosure, lua_setfield, lua_settop,
        },
    };

    use super::{
        ERROR_MARKER, Error, ErrorKind, finish_async, prepare_async, push_native_error,
        state_from_thread, take_async_yield, take_lua_error,
    };

    fn raw_lua() -> lunka::Lua {
        lunka::Lua::try_new().expect("create raw Lua state")
    }

    unsafe extern "C-unwind" fn raw_yield(state: *mut lua_State) -> c_int {
        unsafe { Thread::from_ptr(state).yield_with(0) }
    }

    #[test]
    fn execution_rejects_unmanaged_lua_yields() {
        let mut lua = super::Lua::new().unwrap();
        lua.push_environment();
        unsafe {
            lua_pushcclosure(lua.raw.as_ptr(), raw_yield as lua_CFunction, 0);
            lua_setfield(lua.raw.as_ptr(), -2, c"raw_yield".as_ptr());
            lua_pop(lua.raw.as_ptr(), 1);
        }
        let mut execution = lua.load("raw_yield()").exec_async();
        let result = Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop()));
        assert!(
            matches!(result, Poll::Ready(Err(error)) if error.kind() == ErrorKind::UnexpectedYield)
        );
    }

    #[test]
    fn function_call_rejects_unmanaged_lua_yields() {
        let mut lua = super::Lua::new().unwrap();
        lua.push_environment();
        unsafe {
            lua_pushcclosure(lua.raw.as_ptr(), raw_yield as lua_CFunction, 0);
            lua_setfield(lua.raw.as_ptr(), -2, c"raw_yield".as_ptr());
            lua_pop(lua.raw.as_ptr(), 1);
        }
        let callback: crate::Function = lua
            .load("return function() raw_yield() end")
            .eval()
            .unwrap();
        let mut call = callback.call_async::<_, ()>(());
        let result = Pin::new(&mut call).poll(&mut Context::from_waker(Waker::noop()));
        assert!(
            matches!(result, Poll::Ready(Err(error)) if error.kind() == ErrorKind::UnexpectedYield)
        );
    }

    #[test]
    fn rejects_yields_without_native_task_marker() {
        let mut lua = raw_lua();
        let state = lua.as_ptr();
        let wrong_count = unsafe { take_async_yield(state, 0) }.unwrap_err();
        assert_eq!(wrong_count.kind(), ErrorKind::UnexpectedYield);

        lua.push_nil();
        lua.push_nil();
        let wrong_marker = unsafe { take_async_yield(state, 2) }.unwrap_err();
        assert_eq!(wrong_marker.kind(), ErrorKind::UnexpectedYield);
    }

    #[test]
    fn native_error_marker_is_consumed_by_async_continuation() {
        let mut lua = raw_lua();
        let state = lua.as_ptr();
        unsafe { push_native_error(state, &Error::runtime("native failed")) };
        assert_eq!(unsafe { finish_async(state) }, -1);
        assert_eq!(lua.top(), 1);

        unsafe { lua_settop(state, 0) };
        unsafe { lua.push_light_userdata((&raw const ERROR_MARKER).cast_mut().cast()) };
        assert_eq!(unsafe { finish_async(state) }, 1);
    }

    #[test]
    fn async_preparation_rejects_missing_context_and_callback() {
        let mut lua = raw_lua();
        let state = lua.as_ptr();
        assert_eq!(unsafe { prepare_async(state, core::ptr::null_mut()) }, -1);
        unsafe { lua_settop(state, 0) };

        let mut context = 0;
        assert_eq!(unsafe { prepare_async(state, &mut context) }, -1);
    }

    #[test]
    fn lua_errors_have_fallback_messages_for_empty_and_non_string_stacks() {
        let mut lua = raw_lua();
        let state = lua.as_ptr();
        let empty = unsafe { take_lua_error(state, ErrorKind::Runtime) };
        assert_eq!(empty.message(), "unknown Lua error");

        lua.managed().new_table();
        let non_string = unsafe { take_lua_error(state, ErrorKind::Runtime) };
        assert_eq!(non_string.message(), "unknown Lua error");
    }

    #[test]
    fn rejects_lua_states_not_owned_by_barracuda_lua() {
        let mut lua = raw_lua();
        let state = lua.as_ptr();
        unsafe {
            lua_getextraspace(state, DEFAULT_EXTRA_SPACE)
                .cast::<*const super::State>()
                .write(null());
        }
        let error = match state_from_thread(&mut lua) {
            Ok(_) => panic!("raw Lua state unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(error.message().contains("not owned by barracuda-lua"));
    }
}
