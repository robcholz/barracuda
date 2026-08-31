use alloc::{boxed::Box, rc::Rc};
use core::{
    ffi::c_int,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    ptr::{NonNull, null_mut},
    task::{Context as TaskContext, Poll},
};

use lunka::cdef::auxlib::NO_REF;
use lunka::cdef::{
    MULT_RET, REGISTRY_INDEX, lua_gettop, lua_newthread, lua_pcallk, lua_pushvalue, lua_rawget,
    lua_rawgeti, lua_rawset, lua_resetthread, lua_resume, lua_settop, lua_xmove,
};
use lunka::{Thread, cdef::Type};

use crate::runtime::{
    State, Task, push_native_error, state_from_thread, take_async_yield, take_lua_error,
};
use crate::{Error, ErrorKind, FromLua, FromLuaMulti, IntoLua, IntoLuaMulti, Result};

pub struct Context<'lua> {
    lua: &'lua mut Thread,
}

impl<'lua> Context<'lua> {
    pub(crate) fn new(lua: &'lua mut Thread) -> Self {
        Self { lua }
    }

    pub(crate) fn thread(&mut self) -> &mut Thread {
        self.lua
    }

    pub fn create_table(&mut self) -> Result<Table> {
        self.lua.managed().new_table();
        let table = Table::from_lua(self.lua, -1);
        unsafe { lua_settop(self.lua.as_ptr(), -2) };
        table
    }

    pub fn create_registry_value<V: IntoLua>(&mut self, value: V) -> Result<RegistryKey> {
        value.push_to_lua(self.lua)?;
        LuaRef::take_top(self.lua).map(RegistryKey::new)
    }

    pub fn registry_value<V: FromLua>(&mut self, key: &RegistryKey) -> Result<V> {
        let inner = key
            .inner
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Runtime, "registry key has been removed"))?;
        inner.push(self.lua)?;
        let value = V::from_lua(self.lua, -1);
        unsafe { lua_settop(self.lua.as_ptr(), -2) };
        value
    }

    pub fn remove_registry_value(&mut self, mut key: RegistryKey) -> Result<()> {
        let inner = key
            .inner
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Runtime, "registry key has been removed"))?;
        inner.remove_from(self.lua)
    }
}

pub(crate) struct LuaRef {
    state: Rc<State>,
    reference: c_int,
}

impl LuaRef {
    pub(crate) fn from_stack(lua: &mut Thread, index: c_int) -> Result<Self> {
        unsafe { lua_pushvalue(lua.as_ptr(), index) };
        Self::take_top(lua)
    }

    fn take_top(lua: &mut Thread) -> Result<Self> {
        let state = state_from_thread(lua)?;
        let reference = lua.managed().create_ref(REGISTRY_INDEX);
        Ok(Self { state, reference })
    }

    pub(crate) fn push(&self, lua: &mut Thread) -> Result<()> {
        let target = state_from_thread(lua)?;
        if !Rc::ptr_eq(&self.state, &target) {
            return Err(Error::new(
                ErrorKind::Runtime,
                "Lua value belongs to a different state",
            ));
        }
        unsafe { lua_rawgeti(lua.as_ptr(), REGISTRY_INDEX, self.reference.into()) };
        Ok(())
    }

    pub(crate) fn state(&self) -> Rc<State> {
        Rc::clone(&self.state)
    }

    fn remove_from(mut self, lua: &mut Thread) -> Result<()> {
        let target = state_from_thread(lua)?;
        if !Rc::ptr_eq(&self.state, &target) {
            return Err(Error::new(
                ErrorKind::Runtime,
                "registry key belongs to a different Lua state",
            ));
        }
        lua.destroy_ref(REGISTRY_INDEX, self.reference);
        self.reference = NO_REF;
        Ok(())
    }

    pub(crate) fn with_main<R>(
        &self,
        operation: impl FnOnce(&mut Thread) -> Result<R>,
    ) -> Result<R> {
        let main = self.state.main()?;
        let lua = unsafe { Thread::from_ptr_mut(main) };
        let top = unsafe { lua_gettop(main) };
        let result = operation(lua);
        unsafe { lua_settop(main, top) };
        result
    }
}

impl Drop for LuaRef {
    fn drop(&mut self) {
        if let Ok(main) = self.state.main() {
            unsafe { Thread::from_ptr(main) }.destroy_ref(REGISTRY_INDEX, self.reference);
        }
    }
}

pub struct Table {
    inner: LuaRef,
}

impl Table {
    pub fn get<K: IntoLua, V: FromLua>(&self, key: K) -> Result<V> {
        self.inner.with_main(|lua| {
            self.inner.push(lua)?;
            key.push_to_lua(lua)?;
            unsafe { lua_rawget(lua.as_ptr(), -2) };
            V::from_lua(lua, -1)
        })
    }

    pub fn set<K: IntoLua, V: IntoLua>(&self, key: K, value: V) -> Result<()> {
        self.inner.with_main(|lua| {
            self.inner.push(lua)?;
            key.push_to_lua(lua)?;
            value.push_to_lua(lua)?;
            unsafe { lua_rawset(lua.as_ptr(), -3) };
            Ok(())
        })
    }

    pub fn len(&self) -> Result<usize> {
        self.inner.with_main(|lua| {
            self.inner.push(lua)?;
            usize::try_from(lua.raw_length(-1))
                .map_err(|_| Error::new(ErrorKind::Conversion, "table length does not fit usize"))
        })
    }

    pub fn is_empty(&self) -> Result<bool> {
        self.len().map(|length| length == 0)
    }
}

impl FromLua for Table {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::Table {
            return Err(Error::new(ErrorKind::Conversion, "expected table"));
        }
        LuaRef::from_stack(lua, index).map(|inner| Self { inner })
    }
}

impl IntoLua for Table {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        self.inner.push(lua)
    }
}

pub struct Function {
    inner: LuaRef,
}

impl Function {
    pub fn call<A: IntoLuaMulti, R: FromLuaMulti>(&self, arguments: A) -> Result<R> {
        self.inner.with_main(|lua| {
            let base = lua.top();
            self.inner.push(lua)?;
            let argument_count = arguments.push_to_lua_multi(lua)?;
            let status = unsafe { lua_pcallk(lua.as_ptr(), argument_count, MULT_RET, 0, 0, None) };
            if status != 0 {
                return Err(unsafe { take_lua_error(lua.as_ptr(), ErrorKind::Runtime) });
            }
            R::from_lua_multi(lua, lua.top() - base)
        })
    }

    pub fn call_async<A: IntoLuaMulti + 'static, R: FromLuaMulti>(
        &self,
        arguments: A,
    ) -> FunctionCall<R> {
        FunctionCall::new(self, arguments)
    }
}

pub struct FunctionCall<R> {
    state: Rc<State>,
    coroutine: *mut lunka::cdef::lua_State,
    coroutine_ref: Option<LuaRef>,
    resume_args: c_int,
    pending: Option<NonNull<Task>>,
    init_error: Option<Error>,
    finished: bool,
    result: PhantomData<fn() -> R>,
}

impl<R> FunctionCall<R> {
    fn new<A: IntoLuaMulti + 'static>(function: &Function, arguments: A) -> Self {
        let state = function.inner.state();
        let main = match state.main() {
            Ok(main) => main,
            Err(error) => return Self::failed(state, error),
        };
        let lua = unsafe { Thread::from_ptr_mut(main) };
        let main_top = lua.top();
        let mut coroutine = null_mut();
        let initialized = (|| {
            coroutine = unsafe { lua_newthread(main) };
            let coroutine_ref = LuaRef::from_stack(lua, -1)?;
            function.inner.push(lua)?;
            unsafe { lua_xmove(main, coroutine, 1) };
            let coroutine_thread = unsafe { Thread::from_ptr_mut(coroutine) };
            let argument_count = arguments.push_to_lua_multi(coroutine_thread)?;
            Ok((coroutine_ref, argument_count))
        })();
        unsafe { lua_settop(main, main_top) };

        match initialized {
            Ok((coroutine_ref, resume_args)) => Self {
                state,
                coroutine,
                coroutine_ref: Some(coroutine_ref),
                resume_args,
                pending: None,
                init_error: None,
                finished: false,
                result: PhantomData,
            },
            Err(error) => {
                unsafe {
                    let _ = lua_resetthread(coroutine);
                }
                Self::failed(state, error)
            }
        }
    }

    fn failed(state: Rc<State>, error: Error) -> Self {
        Self {
            state,
            coroutine: null_mut(),
            coroutine_ref: None,
            resume_args: 0,
            pending: None,
            init_error: Some(error),
            finished: false,
            result: PhantomData,
        }
    }

    fn cleanup(&mut self) {
        if self.finished {
            return;
        }
        if let Some(task) = self.pending.take() {
            unsafe { drop(Box::from_raw(task.as_ptr())) };
        }
        if self.state.main().is_ok() && !self.coroutine.is_null() {
            unsafe {
                let _ = lua_resetthread(self.coroutine);
            }
        }
        self.coroutine_ref.take();
        self.finished = true;
    }
}

impl<R> Unpin for FunctionCall<R> {}

impl<R: FromLuaMulti> Future for FunctionCall<R> {
    type Output = Result<R>;

    fn poll(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(error) = this.init_error.take() {
            this.cleanup();
            return Poll::Ready(Err(error));
        }
        if this.finished {
            return Poll::Ready(Err(Error::new(
                ErrorKind::Runtime,
                "function call polled after completion",
            )));
        }
        if let Err(error) = this.state.main() {
            this.cleanup();
            return Poll::Ready(Err(error));
        }

        loop {
            if let Some(mut task_pointer) = this.pending {
                let task = unsafe { task_pointer.as_mut() };
                match task.future.as_mut().poll(context) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(push) => {
                        this.pending = None;
                        unsafe { drop(Box::from_raw(task_pointer.as_ptr())) };
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
                    let lua = unsafe { Thread::from_ptr_mut(this.coroutine) };
                    let output = R::from_lua_multi(lua, result_count);
                    this.cleanup();
                    return Poll::Ready(output);
                }
                1 => match unsafe { take_async_yield(this.coroutine, result_count) } {
                    Ok(task) => this.pending = Some(task),
                    Err(error) => {
                        this.cleanup();
                        return Poll::Ready(Err(error));
                    }
                },
                _ => {
                    let error = unsafe { take_lua_error(this.coroutine, ErrorKind::Runtime) };
                    this.cleanup();
                    return Poll::Ready(Err(error));
                }
            }
        }
    }
}

impl<R> Drop for FunctionCall<R> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl FromLua for Function {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::Function {
            return Err(Error::new(ErrorKind::Conversion, "expected function"));
        }
        LuaRef::from_stack(lua, index).map(|inner| Self { inner })
    }
}

impl IntoLua for Function {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        self.inner.push(lua)
    }
}

pub struct RegistryKey {
    inner: Option<LuaRef>,
    not_send: PhantomData<*mut ()>,
}

impl RegistryKey {
    fn new(inner: LuaRef) -> Self {
        Self {
            inner: Some(inner),
            not_send: PhantomData,
        }
    }
}
