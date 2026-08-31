use alloc::{boxed::Box, ffi::CString, format};
use core::{
    cell::{Cell, UnsafeCell},
    ffi::c_int,
    future::Future,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{NonNull, null_mut},
};

use lunka::Thread;
use lunka::cdef::auxlib::{luaL_setmetatable, luaL_testudata};
use lunka::cdef::{
    REGISTRY_INDEX, lua_CFunction, lua_State, lua_pushcclosure, lua_pushvalue, lua_setfield,
    lua_settop, lua_touserdata,
};

use crate::object::LuaRef;
use crate::runtime::{Callback, NativeFuture, ReturnPusher, push_lua_return, state_from_thread};
use crate::{
    Context, Error, ErrorKind, FromLua, FromLuaMulti, IntoLua, IntoLuaMulti, LuaReturn, Result,
};

pub trait UserData: 'static {
    fn add_methods(_methods: &mut UserDataMethods<'_, Self>)
    where
        Self: Sized,
    {
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetaMethod {
    Close,
    ToString,
    Eq,
}

impl MetaMethod {
    fn name(self) -> &'static str {
        match self {
            Self::Close => "__close",
            Self::ToString => "__tostring",
            Self::Eq => "__eq",
        }
    }
}

pub struct UserDataMethods<'lua, T: UserData> {
    lua: &'lua mut Thread,
    methods_index: c_int,
    metatable_index: c_int,
    error: Option<Error>,
    marker: PhantomData<fn() -> T>,
}

impl<T: UserData> UserDataMethods<'_, T> {
    pub fn add_method<A, R, F>(&mut self, name: &str, function: F)
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&T, A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count: c_int| {
            if count < 1 {
                return Err(Error::new(ErrorKind::Conversion, "expected userdata self"));
            }
            let arguments = A::from_lua_multi(lua, count - 1)?;
            let cell = unsafe { cell_from_lua::<T>(lua, 1)?.as_ref() };
            let value_ref = cell.borrow()?;
            let value = function(&value_ref, arguments);
            push_lua_return(lua, value)
        };
        self.install(name, self.methods_index, Callback::Sync(Box::new(callback)));
    }

    pub fn add_method_mut<A, R, F>(&mut self, name: &str, function: F)
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut T, A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count: c_int| {
            if count < 1 {
                return Err(Error::new(ErrorKind::Conversion, "expected userdata self"));
            }
            let arguments = A::from_lua_multi(lua, count - 1)?;
            let cell = unsafe { cell_from_lua::<T>(lua, 1)?.as_ref() };
            let mut value_ref = cell.borrow_mut()?;
            let value = function(&mut value_ref, arguments);
            push_lua_return(lua, value)
        };
        self.install(name, self.methods_index, Callback::Sync(Box::new(callback)));
    }

    pub fn add_async_method<A, R, F, Fut>(&mut self, name: &str, function: F)
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(UserDataHandle<T>, A) -> Fut + 'static,
        Fut: Future<Output = LuaReturn<R>> + 'static,
    {
        let callback = move |lua: &mut Thread, count: c_int| {
            if count < 1 {
                return Err(Error::new(ErrorKind::Conversion, "expected userdata self"));
            }
            let arguments = A::from_lua_multi(lua, count - 1)?;
            let userdata = UserDataHandle::<T>::from_lua(lua, 1)?;
            let future = function(userdata, arguments);
            let future: NativeFuture = Box::pin(async move {
                let value = future.await;
                let pusher: ReturnPusher = Box::new(move |lua| push_lua_return(lua, value));
                pusher
            });
            Ok(future)
        };
        self.install(
            name,
            self.methods_index,
            Callback::Async(Box::new(callback)),
        );
    }

    pub fn add_meta_method<A, R, F>(&mut self, method: MetaMethod, function: F)
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&T, A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count: c_int| {
            if count < 1 {
                return Err(Error::new(ErrorKind::Conversion, "expected userdata self"));
            }
            let arguments = A::from_lua_multi(lua, count - 1)?;
            let cell = unsafe { cell_from_lua::<T>(lua, 1)?.as_ref() };
            let value_ref = cell.borrow()?;
            let value = function(&value_ref, arguments);
            push_lua_return(lua, value)
        };
        self.install(
            method.name(),
            self.metatable_index,
            Callback::Sync(Box::new(callback)),
        );
    }

    pub fn add_meta_method_mut<A, R, F>(&mut self, method: MetaMethod, function: F)
    where
        A: FromLuaMulti + 'static,
        R: IntoLuaMulti + 'static,
        F: Fn(&mut T, A) -> LuaReturn<R> + 'static,
    {
        let callback = move |lua: &mut Thread, count: c_int| {
            if count < 1 {
                return Err(Error::new(ErrorKind::Conversion, "expected userdata self"));
            }
            let arguments = A::from_lua_multi(lua, count - 1)?;
            let cell = unsafe { cell_from_lua::<T>(lua, 1)?.as_ref() };
            let mut value_ref = cell.borrow_mut()?;
            let value = function(&mut value_ref, arguments);
            push_lua_return(lua, value)
        };
        self.install(
            method.name(),
            self.metatable_index,
            Callback::Sync(Box::new(callback)),
        );
    }

    fn install(&mut self, name: &str, table_index: c_int, callback: Callback) {
        if self.error.is_some() {
            return;
        }
        let name = match CString::new(name) {
            Ok(name) => name,
            Err(_) => {
                self.error = Some(Error::new(
                    ErrorKind::Conversion,
                    "userdata method name contains NUL",
                ));
                return;
            }
        };
        let result = (|| {
            let state = state_from_thread(self.lua)?;
            let index = state.store_callback(callback);
            state.push_callback(self.lua, index)?;
            unsafe { lua_setfield(self.lua.as_ptr(), table_index, name.as_ptr()) };
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
}

impl Context<'_> {
    pub fn create_userdata<T: UserData>(&mut self, value: T) -> Result<UserDataHandle<T>> {
        let type_name = userdata_type_name::<T>()?;
        let state = state_from_thread(self.thread())?;
        let callback_count = state.callback_count();
        let initial_top = self.thread().top();
        let new_metatable = self.thread().managed().new_metatable(&type_name);
        let metatable_index = self.thread().top();

        if new_metatable {
            self.thread().managed().new_table();
            let methods_index = self.thread().top();
            unsafe {
                lua_pushvalue(self.thread().as_ptr(), methods_index);
                lua_setfield(self.thread().as_ptr(), metatable_index, c"__index".as_ptr());
                lua_pushcclosure(self.thread().as_ptr(), userdata_gc::<T> as lua_CFunction, 0);
                lua_setfield(self.thread().as_ptr(), metatable_index, c"__gc".as_ptr());
            }
            let mut methods = UserDataMethods {
                lua: self.thread(),
                methods_index,
                metatable_index,
                error: None,
                marker: PhantomData,
            };
            T::add_methods(&mut methods);
            if let Some(error) = methods.error {
                methods.lua.push_nil();
                unsafe {
                    lua_setfield(methods.lua.as_ptr(), REGISTRY_INDEX, type_name.as_ptr());
                    lua_settop(methods.lua.as_ptr(), initial_top);
                }
                state.truncate_callbacks(callback_count);
                return Err(error);
            }
        }
        unsafe { lua_settop(self.thread().as_ptr(), initial_top) };

        let slot = unsafe {
            self.thread()
                .managed()
                .new_userdata_raw(core::mem::size_of::<*mut UserDataCell<T>>(), 0)
                .cast::<*mut UserDataCell<T>>()
        };
        let pointer = Box::into_raw(Box::new(UserDataCell::new(value)));
        unsafe {
            slot.write(pointer);
            luaL_setmetatable(self.thread().as_ptr(), type_name.as_ptr());
        }
        let userdata = UserDataHandle::from_lua(self.thread(), -1);
        unsafe { lua_settop(self.thread().as_ptr(), -2) };
        userdata
    }
}

pub struct UserDataHandle<T: UserData> {
    inner: LuaRef,
    marker: PhantomData<fn() -> T>,
}

impl<T: UserData> UserDataHandle<T> {
    pub fn borrow(&self) -> Result<UserDataRef<'_, T>> {
        let pointer = self.inner.with_main(|lua| {
            self.inner.push(lua)?;
            cell_from_lua::<T>(lua, -1)
        })?;
        unsafe { pointer.as_ref() }.borrow()
    }

    pub fn borrow_mut(&self) -> Result<UserDataRefMut<'_, T>> {
        let pointer = self.inner.with_main(|lua| {
            self.inner.push(lua)?;
            cell_from_lua::<T>(lua, -1)
        })?;
        unsafe { pointer.as_ref() }.borrow_mut()
    }

    pub fn with<R>(&self, function: impl FnOnce(&T) -> R) -> Result<R> {
        self.borrow().map(|value| function(&value))
    }

    pub fn with_mut<R>(&self, function: impl FnOnce(&mut T) -> R) -> Result<R> {
        self.borrow_mut().map(|mut value| function(&mut value))
    }
}

impl<T: UserData> FromLua for UserDataHandle<T> {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        let _ = cell_from_lua::<T>(lua, index)?;
        LuaRef::from_stack(lua, index).map(|inner| Self {
            inner,
            marker: PhantomData,
        })
    }
}

impl<T: UserData> IntoLua for UserDataHandle<T> {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        self.inner.push(lua)
    }
}

pub struct UserDataRef<'a, T> {
    cell: NonNull<UserDataCell<T>>,
    marker: PhantomData<&'a T>,
}

impl<T> Deref for UserDataRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.as_ref().value.get() }
    }
}

impl<T> Drop for UserDataRef<'_, T> {
    fn drop(&mut self) {
        let cell = unsafe { self.cell.as_ref() };
        cell.borrows.set(cell.borrows.get().saturating_sub(1));
    }
}

pub struct UserDataRefMut<'a, T> {
    cell: NonNull<UserDataCell<T>>,
    marker: PhantomData<&'a mut T>,
}

impl<T> Deref for UserDataRefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.as_ref().value.get() }
    }
}

impl<T> DerefMut for UserDataRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.cell.as_mut().value.get() }
    }
}

impl<T> Drop for UserDataRefMut<'_, T> {
    fn drop(&mut self) {
        unsafe { self.cell.as_ref() }.borrows.set(0);
    }
}

struct UserDataCell<T> {
    value: UnsafeCell<T>,
    borrows: Cell<isize>,
}

impl<T> UserDataCell<T> {
    fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            borrows: Cell::new(0),
        }
    }

    fn borrow(&self) -> Result<UserDataRef<'_, T>> {
        let borrows = self.borrows.get();
        if borrows < 0 {
            return Err(Error::new(
                ErrorKind::Runtime,
                "userdata is already mutably borrowed",
            ));
        }
        let borrows = borrows
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::Runtime, "too many userdata borrows"))?;
        self.borrows.set(borrows);
        Ok(UserDataRef {
            cell: NonNull::from(self),
            marker: PhantomData,
        })
    }

    fn borrow_mut(&self) -> Result<UserDataRefMut<'_, T>> {
        if self.borrows.get() != 0 {
            return Err(Error::new(
                ErrorKind::Runtime,
                "userdata is already borrowed",
            ));
        }
        self.borrows.set(-1);
        Ok(UserDataRefMut {
            cell: NonNull::from(self),
            marker: PhantomData,
        })
    }
}

fn userdata_type_name<T: UserData>() -> Result<CString> {
    CString::new(format!("barracuda-lua:{}", core::any::type_name::<T>()))
        .map_err(|_| Error::new(ErrorKind::Conversion, "userdata type name contains NUL"))
}

fn cell_from_lua<T: UserData>(lua: &mut Thread, index: c_int) -> Result<NonNull<UserDataCell<T>>> {
    let type_name = userdata_type_name::<T>()?;
    let slot = unsafe { luaL_testudata(lua.as_ptr(), index, type_name.as_ptr()) }
        .cast::<*mut UserDataCell<T>>();
    if slot.is_null() {
        return Err(Error::new(
            ErrorKind::Conversion,
            format!("expected {} userdata", core::any::type_name::<T>()),
        ));
    }
    let pointer = unsafe { slot.read() };
    NonNull::new(pointer)
        .ok_or_else(|| Error::new(ErrorKind::Runtime, "userdata has been finalized"))
}

unsafe extern "C-unwind" fn userdata_gc<T: UserData>(state: *mut lua_State) -> c_int {
    let slot = unsafe { lua_touserdata(state, 1) }.cast::<*mut UserDataCell<T>>();
    if slot.is_null() {
        return 0;
    }
    let pointer = unsafe { slot.replace(null_mut()) };
    if !pointer.is_null() {
        unsafe { drop(Box::from_raw(pointer)) };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::UserDataCell;

    #[test]
    fn mutable_reference_supports_shared_deref_and_rejects_other_borrows() {
        let cell = UserDataCell::new(1_i64);
        let mut value = cell.borrow_mut().unwrap();
        assert_eq!(*value, 1);
        *value = 2;
        assert!(cell.borrow().is_err());
        assert!(cell.borrow_mut().is_err());
        drop(value);
        assert_eq!(*cell.borrow().unwrap(), 2);
    }
}
