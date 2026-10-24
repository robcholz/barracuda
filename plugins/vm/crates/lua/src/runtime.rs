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

use crate::ffi::{
    DEFAULT_EXTRA_SPACE, GC_COLLECT, LOADED_TABLE, MASK_COUNT, MAX_ALIGN, MIN_STACK, MainThread,
    PRELOAD_TABLE, REGISTRY_GLOBALS, REGISTRY_INDEX, STATUS_MEMORY_ERROR, Thread, Type,
    barracuda_lua_memory_error, barracuda_lua_open_require, lua_CFunction, lua_Debug, lua_KContext,
    lua_State, lua_checkstack, lua_createtable, lua_error, lua_gc, lua_getallocf,
    lua_getextraspace, lua_getfield, lua_getglobal, lua_gettop, lua_newthread, lua_pcallk, lua_pop,
    lua_pushcclosure, lua_pushlightuserdata, lua_pushlstring, lua_pushnil, lua_pushvalue,
    lua_rawgeti, lua_rawset, lua_rawseti, lua_remove, lua_resetthread, lua_resume, lua_setfield,
    lua_sethook, lua_settop, lua_setupvalue, lua_tolstring, lua_touserdata, lua_xmove,
    luaL_loadbufferx, luaL_ref, luaL_requiref, luaopen_base, luaopen_string, luaopen_table,
    luaopen_utf8,
};

use crate::{
    Context as LuaContext, Error, ErrorKind, FromLuaMulti, IntoLua, IntoLuaMulti, LuaReturn,
    Package, RegistryKey, Result,
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

type FindPackage = Box<dyn Fn(&str) -> Option<Box<dyn Package>> + 'static>;

/// Finds the package a script requires by a name nothing is registered under.
struct PackageFinder {
    headroom: usize,
    find: FindPackage,
}

/// Most packages installing inside one another's installation at once.
const MAX_NESTED_INSTALLS: u32 = 8;

pub(crate) struct Task {
    pub(crate) future: NativeFuture,
}

static ASYNC_MARKER: u8 = 0xA5;
static ERROR_MARKER: u8 = 0xE1;

const ENVIRONMENT_GLOBALS: [&core::ffi::CStr; 22] = [
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
    c"utf8",
    c"_VERSION",
];

pub struct Lua {
    raw: Root,
    state: Rc<State>,
}

/// The thread a [`Lua`] works on.
enum Root {
    /// The state's main thread, which the `Lua` owns and closes.
    Main(MainThread),
    /// The thread whose `require` is installing a package; it belongs to the
    /// running script.
    Requiring(&'static mut Thread),
}

impl core::ops::Deref for Root {
    type Target = Thread;

    fn deref(&self) -> &Thread {
        match self {
            Root::Main(main) => main,
            Root::Requiring(thread) => thread,
        }
    }
}

impl core::ops::DerefMut for Root {
    fn deref_mut(&mut self) -> &mut Thread {
        match self {
            Root::Main(main) => main,
            Root::Requiring(thread) => thread,
        }
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
        lua_pop(state, 1);

        lua_rawgeti(state, REGISTRY_INDEX, environment.into());
        lua_rawseti(state, REGISTRY_INDEX, REGISTRY_GLOBALS);
    }
}

pub(crate) struct State {
    main: Cell<*mut lua_State>,
    environment: c_int,
    callbacks: RefCell<Vec<Rc<Callback>>>,
    instruction_hook: RefCell<Option<InstructionHook>>,
    instruction_hook_interval: Cell<c_int>,
    hook_yielded: Cell<bool>,
    package_finder: RefCell<Option<Rc<PackageFinder>>>,
    /// Packages installing on first require right now, one inside another.
    installing: Cell<u32>,
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
        // The borrow ends before Lua allocates, which can raise an error that
        // skips its release.
        let (callback_ptr, dispatcher) = {
            let callbacks = self.callbacks.borrow();
            let callback = callbacks.get(index).ok_or_else(|| {
                Error::new(ErrorKind::Runtime, "native callback index is invalid")
            })?;
            let dispatcher: lua_CFunction = match callback.as_ref() {
                Callback::Sync(_) => sync_dispatch,
                Callback::Async(_) => async_dispatch,
            };
            (Rc::as_ptr(callback).cast_mut().cast::<c_void>(), dispatcher)
        };
        unsafe {
            lua_pushlightuserdata(lua.as_ptr(), callback_ptr);
            lua_pushcclosure(lua.as_ptr(), dispatcher, 1);
        }
        Ok(())
    }

    /// Sets the hook of a new coroutine. Chunks a package runs while it
    /// installs on first require get none: their `require` cannot yield.
    fn install_instruction_hook(&self, coroutine: *mut lua_State) {
        let interval = self.instruction_hook_interval.get();
        let enabled =
            self.instruction_hook.borrow().is_some() && interval > 0 && self.installing.get() == 0;
        let hook = enabled.then_some(instruction_hook_dispatch as _);
        let mask = if enabled { MASK_COUNT } else { 0 };
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

/// The program's global allocator, for states created by [`Lua::new`].
struct GlobalAllocator;

unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { alloc::alloc::alloc(layout) }
    }

    unsafe fn dealloc(&self, allocation: *mut u8, layout: Layout) {
        unsafe { alloc::alloc::dealloc(allocation, layout) }
    }

    unsafe fn realloc(&self, allocation: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { alloc::alloc::realloc(allocation, layout, new_size) }
    }
}

static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator;

/// A bare state on the global allocator.
fn new_main_thread() -> Option<MainThread> {
    unsafe {
        MainThread::new(
            rust_allocator::<GlobalAllocator>,
            (&raw const GLOBAL_ALLOCATOR).cast_mut().cast::<c_void>(),
        )
    }
}

impl Lua {
    pub fn new() -> Result<Self> {
        let raw = new_main_thread()
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
        let raw =
            unsafe { MainThread::new(rust_allocator::<A>, allocator.cast_mut().cast::<c_void>()) }
                .ok_or_else(|| {
                    Error::new(ErrorKind::Memory, "external Lua allocator is exhausted")
                })?;
        Self::initialize(raw)
    }

    fn initialize(raw: MainThread) -> Result<Self> {
        let state = raw.as_ptr();
        unsafe {
            luaL_requiref(state, c"_G".as_ptr(), luaopen_base, 1);
            lua_pop(state, 1);
            luaL_requiref(state, c"string".as_ptr(), luaopen_string, 1);
            lua_pop(state, 1);
            luaL_requiref(state, c"table".as_ptr(), luaopen_table, 1);
            lua_pop(state, 1);
            luaL_requiref(state, c"utf8".as_ptr(), luaopen_utf8, 1);
            lua_pop(state, 1);
            barracuda_lua_open_require(state, Some(require_package));
        }
        let environment = unsafe { create_environment(state) };
        unsafe { discard_bootstrap_environment(state, environment) };
        let shared = Rc::new(State {
            main: Cell::new(state),
            environment,
            callbacks: RefCell::new(Vec::new()),
            instruction_hook: RefCell::new(None),
            instruction_hook_interval: Cell::new(0),
            hook_yielded: Cell::new(false),
            package_finder: RefCell::new(None),
            installing: Cell::new(0),
        });
        unsafe {
            let slot = lua_getextraspace(state, DEFAULT_EXTRA_SPACE).cast::<*const State>();
            slot.write(Rc::as_ptr(&shared));
        }
        Ok(Self {
            raw: Root::Main(raw),
            state: shared,
        })
    }

    /// Runs a full garbage collection cycle.
    pub fn collect_garbage(&mut self) {
        unsafe { lua_gc(self.raw.as_ptr(), GC_COLLECT) };
    }

    /// Lets scripts require packages that install only when first required.
    ///
    /// When a script requires a name that is neither loaded nor registered
    /// with [`Lua::register_lib`], `find` is asked for a package by that
    /// name. The package it returns installs into this state during that
    /// `require`, which then looks the name up again: the package registers
    /// its library under the name, and the script gets what it would have
    /// had if the package had installed before the script ran. When `find`
    /// returns `None`, or the package registers nothing under the name, the
    /// `require` fails as for any unknown module.
    ///
    /// Before a package installs, `headroom` bytes must be free in one block
    /// of the state's allocator, after a full garbage collection if needed;
    /// otherwise the `require` raises Lua's out-of-memory error without
    /// installing anything. An installation's own failure, including running
    /// out of memory, is an error raised by that `require`. Chunks the
    /// package runs while it installs run without the instruction hook.
    pub fn set_package_finder<F>(&mut self, headroom: usize, find: F)
    where
        F: Fn(&str) -> Option<Box<dyn Package>> + 'static,
    {
        self.state
            .package_finder
            .replace(Some(Rc::new(PackageFinder {
                headroom,
                find: Box::new(find),
            })));
    }

    /// Returns the bytes this state currently has allocated.
    #[must_use]
    pub fn memory_in_use(&mut self) -> usize {
        const LUA_GCCOUNT: c_int = 3;
        const LUA_GCCOUNTB: c_int = 4;
        let state = self.raw.as_ptr();
        let kibibytes = unsafe { crate::ffi::lua_gc(state, LUA_GCCOUNT) };
        let bytes = unsafe { crate::ffi::lua_gc(state, LUA_GCCOUNTB) };
        usize::try_from(kibibytes)
            .unwrap_or(0)
            .saturating_mul(1024)
            .saturating_add(usize::try_from(bytes).unwrap_or(0))
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
        self.raw.new_table();
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
        if self.raw.type_of(-1) != Type::Table {
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
            lua_rawgeti(
                self.raw.as_ptr(),
                REGISTRY_INDEX,
                self.state.environment.into(),
            );
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
        if matches!(self.raw, Root::Main(_)) {
            self.state.main.set(null_mut());
        }
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
        self.lua.raw.new_table();
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
            let kind = if load_status == STATUS_MEMORY_ERROR {
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
                    let kind = if status == STATUS_MEMORY_ERROR {
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
            lua.push_string(error.message().as_bytes());
            Ok(2)
        }
    }
}

unsafe extern "C-unwind" fn preload_loader(state: *mut lua_State) -> c_int {
    unsafe { lua_pushvalue(state, crate::ffi::lua_upvalueindex(1)) };
    1
}

/// What [`install_required`] did with a name nothing was registered under.
enum Required {
    /// No package by the name; `require` reports an unknown module.
    Nothing,
    /// The package installed.
    Installed,
    /// The package did not install; the error is on the stack.
    Failed,
    /// The heap lacked the headroom, or the installation ran out of memory.
    OutOfMemory,
    /// Packages were already installing inside one another too deeply.
    TooDeep,
}

/// Called by `require` (`c/require.c`) with the name at index 1: installs
/// the package the state's [`PackageFinder`] finds by that name.
///
/// Lua errors raised here skip Rust destructors, so each is raised only after
/// [`install_required`] has returned and dropped its values.
unsafe extern "C-unwind" fn require_package(state: *mut lua_State) -> c_int {
    match unsafe { install_required(state) } {
        Required::Nothing | Required::Installed => 0,
        Required::Failed => unsafe { lua_error(state) },
        Required::OutOfMemory => unsafe { barracuda_lua_memory_error(state) },
        Required::TooDeep => unsafe {
            push_error_bytes(state, b"Lua packages install inside one another too deeply");
            lua_error(state)
        },
    }
}

/// One package installing on first require, owned by [`install_required`]'s
/// frame so that an error inside the installation skips none of it.
struct RequiredInstall<'a> {
    package: &'a dyn Package,
    lua: Lua,
}

unsafe fn install_required(state: *mut lua_State) -> Required {
    let thread = unsafe { Thread::from_ptr_mut(state) };
    let Ok(shared) = state_from_thread(thread) else {
        return Required::Nothing;
    };
    let Some(finder) = shared.package_finder.borrow().clone() else {
        return Required::Nothing;
    };
    let Some(package) = thread
        .to_string(1)
        .and_then(|name| core::str::from_utf8(name).ok())
        .and_then(|name| (finder.find)(name))
    else {
        return Required::Nothing;
    };
    let installing = shared.installing.get();
    if installing >= MAX_NESTED_INSTALLS {
        return Required::TooDeep;
    }
    if !unsafe { has_headroom(state, finder.headroom) } {
        unsafe { lua_gc(state, GC_COLLECT) };
        if !unsafe { has_headroom(state, finder.headroom) } {
            return Required::OutOfMemory;
        }
    }

    let mut install = RequiredInstall {
        package: &*package,
        lua: Lua {
            raw: Root::Requiring(unsafe { Thread::from_ptr_mut(state) }),
            state: Rc::clone(&shared),
        },
    };
    shared.installing.set(installing.saturating_add(1));
    let status = unsafe {
        lua_pushcclosure(state, install_package, 0);
        lua_pushlightuserdata(state, (&raw mut install).cast());
        lua_pcallk(state, 1, 0, 0, 0, None)
    };
    shared.installing.set(installing);
    if status != 0 {
        unsafe { forget_required(state) };
    }
    match status {
        0 => Required::Installed,
        STATUS_MEMORY_ERROR => Required::OutOfMemory,
        _ => Required::Failed,
    }
}

/// Forgets what a failed installation registered or loaded under the name at
/// index 1, so that the name is not found half installed and a later
/// `require` installs the package afresh. Clearing a field allocates nothing.
unsafe fn forget_required(state: *mut lua_State) {
    for table in [PRELOAD_TABLE, LOADED_TABLE] {
        unsafe {
            lua_getfield(state, REGISTRY_INDEX, table.as_ptr());
            lua_pushvalue(state, 1);
            lua_pushnil(state);
            lua_rawset(state, -3);
            lua_pop(state, 1);
        }
    }
}

/// Runs one [`RequiredInstall`], passed as light userdata at index 1, inside
/// [`install_required`]'s protected call.
unsafe extern "C-unwind" fn install_package(state: *mut lua_State) -> c_int {
    // An installation used to run on the main thread, which starts with
    // twice Lua's minimum stack.
    if unsafe { lua_checkstack(state, MIN_STACK.saturating_mul(2)) } == 0 {
        unsafe { barracuda_lua_memory_error(state) };
    }
    let install = unsafe { &mut *lua_touserdata(state, 1).cast::<RequiredInstall<'_>>() };
    match install.package.install(&mut install.lua) {
        Ok(()) => 0,
        Err(error) if error.kind() == ErrorKind::Memory => {
            drop(error);
            unsafe { barracuda_lua_memory_error(state) }
        }
        Err(error) => unsafe { raise_lua_error(state, error) },
    }
}

/// Whether the state's allocator can supply `bytes` in one block.
unsafe fn has_headroom(state: *mut lua_State, bytes: usize) -> bool {
    if bytes == 0 {
        return true;
    }
    let mut userdata = null_mut();
    let allocate = unsafe { lua_getallocf(state, &mut userdata) };
    let block = unsafe { allocate(userdata, null_mut(), 0, bytes) };
    if block.is_null() {
        return false;
    }
    unsafe { allocate(userdata, block, bytes, 0) };
    true
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
        unsafe { lua_touserdata(state, crate::ffi::lua_upvalueindex(1)) as *const Callback };
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

    use crate::ffi::{
        DEFAULT_EXTRA_SPACE, MainThread, Thread, lua_CFunction, lua_State, lua_getextraspace,
        lua_pop, lua_pushcclosure, lua_setfield, lua_settop,
    };

    use super::{
        ERROR_MARKER, Error, ErrorKind, finish_async, prepare_async, push_native_error,
        state_from_thread, take_async_yield, take_lua_error,
    };

    fn raw_lua() -> MainThread {
        super::new_main_thread().expect("create raw Lua state")
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
        let lua = raw_lua();
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
        let lua = raw_lua();
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
        let lua = raw_lua();
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

        lua.new_table();
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
