use alloc::{string::String, vec::Vec};
use core::{
    ffi::c_int,
    ops::{Deref, DerefMut},
};

use crate::ffi::{Thread, Type};
use barracuda_bulk_memory::BulkVec;

use crate::{Error, ErrorKind, Result};

pub trait FromLua: Sized {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self>;
}

pub trait IntoLua {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()>;
}

pub trait FromLuaMulti: Sized {
    fn from_lua_multi(lua: &mut Thread, count: c_int) -> Result<Self>;
}

pub trait IntoLuaMulti {
    fn push_to_lua_multi(self, lua: &mut Thread) -> Result<c_int>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Variadic<T>(Vec<T>);

impl<T> From<Vec<T>> for Variadic<T> {
    fn from(values: Vec<T>) -> Self {
        Self(values)
    }
}

impl<T> From<Variadic<T>> for Vec<T> {
    fn from(values: Variadic<T>) -> Self {
        values.0
    }
}

impl<T> Deref for Variadic<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Variadic<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn wrong_type(expected: &'static str) -> Error {
    Error::new(ErrorKind::Conversion, alloc::format!("expected {expected}"))
}

impl FromLua for bool {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::Boolean {
            return Err(wrong_type("boolean"));
        }
        Ok(lua.to_boolean(index))
    }
}

impl IntoLua for bool {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_boolean(self);
        Ok(())
    }
}

impl FromLua for i64 {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if !lua.is_integer(index) {
            return Err(wrong_type("integer"));
        }
        Ok(lua.to_integer(index))
    }
}

impl IntoLua for i64 {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_integer(self);
        Ok(())
    }
}

impl FromLua for f64 {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::Number {
            return Err(wrong_type("number"));
        }
        Ok(lua.to_number(index))
    }
}

impl IntoLua for f64 {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_number(self);
        Ok(())
    }
}

/// A Lua number, keeping Lua's integer and float subtypes apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    /// A Lua integer.
    Integer(i64),
    /// A Lua float.
    Float(f64),
}

impl FromLua for Number {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::Number {
            return Err(wrong_type("number"));
        }
        if lua.is_integer(index) {
            Ok(Self::Integer(lua.to_integer(index)))
        } else {
            Ok(Self::Float(lua.to_number(index)))
        }
    }
}

impl IntoLua for Number {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        match self {
            Self::Integer(value) => lua.push_integer(value),
            Self::Float(value) => lua.push_number(value),
        }
        Ok(())
    }
}

impl FromLua for String {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::String {
            return Err(wrong_type("string"));
        }
        let bytes = lua
            .to_string(index)
            .ok_or_else(|| wrong_type("string"))?
            .to_vec();
        String::from_utf8(bytes)
            .map_err(|_| Error::new(ErrorKind::Conversion, "expected UTF-8 string"))
    }
}

impl IntoLua for String {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_string(self.as_bytes());
        Ok(())
    }
}

impl IntoLua for &str {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_string(self.as_bytes());
        Ok(())
    }
}

impl FromLua for Vec<u8> {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::String {
            return Err(wrong_type("string"));
        }
        Ok(lua
            .to_string(index)
            .ok_or_else(|| wrong_type("string"))?
            .to_vec())
    }
}

impl IntoLua for Vec<u8> {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_string(&self);
        Ok(())
    }
}

/// Byte string held in Platform bulk memory while crossing the Lua boundary.
///
/// Use this instead of `Vec<u8>` for payloads that may be large (PCM, frames,
/// file contents, bus transfers), so they never occupy the ordinary heap.
#[derive(Debug, Default)]
pub struct Bytes(BulkVec<u8>);

impl Bytes {
    /// Allocates an empty byte string with room for `capacity` bytes.
    ///
    /// # Errors
    ///
    /// Returns a memory error when bulk memory cannot supply the buffer.
    pub fn with_capacity(capacity: usize) -> Result<Self> {
        BulkVec::try_with_capacity(capacity)
            .map(Self)
            .map_err(|_error| Error::memory("byte buffer could not be allocated"))
    }

    /// Allocates a zero-filled byte string of `length` bytes.
    ///
    /// # Errors
    ///
    /// Returns a memory error when bulk memory cannot supply the buffer.
    pub fn zeroed(length: usize) -> Result<Self> {
        let mut bytes = Self::with_capacity(length)?;
        bytes.0.resize(length, 0);
        Ok(bytes)
    }

    /// Copies `bytes` into a new bulk-memory byte string.
    ///
    /// # Errors
    ///
    /// Returns a memory error when bulk memory cannot supply the buffer.
    pub fn copy_from(bytes: &[u8]) -> Result<Self> {
        let mut copy = Self::with_capacity(bytes.len())?;
        copy.0.extend_from_slice(bytes);
        Ok(copy)
    }

    /// Appends bytes within the already reserved capacity when possible.
    ///
    /// # Errors
    ///
    /// Returns a memory error when growing the buffer fails.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<()> {
        self.0
            .try_reserve(bytes.len())
            .map_err(|_error| Error::memory("byte buffer could not grow"))?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    /// Shortens the byte string to `length` bytes.
    pub fn truncate(&mut self, length: usize) {
        if length < self.0.len() {
            self.0.resize(length, 0);
        }
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Bytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl FromLua for Bytes {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) != Type::String {
            return Err(wrong_type("string"));
        }
        let bytes = lua.to_string(index).ok_or_else(|| wrong_type("string"))?;
        Self::copy_from(bytes)
    }
}

impl IntoLua for Bytes {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_string(&*self);
        Ok(())
    }
}

impl IntoLua for &[u8] {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        lua.push_string(self);
        Ok(())
    }
}

impl<T: FromLua> FromLua for Option<T> {
    fn from_lua(lua: &mut Thread, index: c_int) -> Result<Self> {
        if lua.type_of(index) == Type::Nil {
            Ok(None)
        } else {
            T::from_lua(lua, index).map(Some)
        }
    }
}

impl<T: IntoLua> IntoLua for Option<T> {
    fn push_to_lua(self, lua: &mut Thread) -> Result<()> {
        match self {
            Some(value) => value.push_to_lua(lua),
            None => {
                lua.push_nil();
                Ok(())
            }
        }
    }
}

impl FromLuaMulti for () {
    fn from_lua_multi(_lua: &mut Thread, count: c_int) -> Result<Self> {
        if count == 0 {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Conversion, "expected no values"))
        }
    }
}

impl IntoLuaMulti for () {
    fn push_to_lua_multi(self, _lua: &mut Thread) -> Result<c_int> {
        Ok(0)
    }
}

impl<T: FromLua> FromLuaMulti for T {
    fn from_lua_multi(lua: &mut Thread, count: c_int) -> Result<Self> {
        if count != 1 {
            return Err(Error::new(ErrorKind::Conversion, "expected one value"));
        }
        let index = lua.top();
        T::from_lua(lua, index)
    }
}

impl<T: IntoLua> IntoLuaMulti for T {
    fn push_to_lua_multi(self, lua: &mut Thread) -> Result<c_int> {
        self.push_to_lua(lua)?;
        Ok(1)
    }
}

impl<T: FromLua> FromLuaMulti for Variadic<T> {
    fn from_lua_multi(lua: &mut Thread, count: c_int) -> Result<Self> {
        let capacity = usize::try_from(count)
            .map_err(|_| Error::new(ErrorKind::Conversion, "invalid Lua value count"))?;
        let mut values = Vec::with_capacity(capacity);
        let first = lua.top() - count + 1;
        for offset in 0..count {
            values.push(T::from_lua(lua, first + offset)?);
        }
        Ok(Self(values))
    }
}

impl<T: IntoLua> IntoLuaMulti for Variadic<T> {
    fn push_to_lua_multi(self, lua: &mut Thread) -> Result<c_int> {
        let count = c_int::try_from(self.0.len())
            .map_err(|_| Error::new(ErrorKind::Conversion, "too many Lua return values"))?;
        for value in self.0 {
            value.push_to_lua(lua)?;
        }
        Ok(count)
    }
}

macro_rules! tuple_impl {
    ($count:expr; $(($type:ident, $value:ident, $offset:expr)),+) => {
        impl<$($type: FromLua),+> FromLuaMulti for ($($type,)+) {
            fn from_lua_multi(lua: &mut Thread, count: c_int) -> Result<Self> {
                if count != $count {
                    return Err(Error::new(
                        ErrorKind::Conversion,
                        alloc::format!("expected {} values", $count),
                    ));
                }
                let first = lua.top() - count + 1;
                $(let $value = $type::from_lua(lua, first + $offset)?;)+
                Ok(($($value,)+))
            }
        }

        impl<$($type: IntoLua),+> IntoLuaMulti for ($($type,)+) {
            fn push_to_lua_multi(self, lua: &mut Thread) -> Result<c_int> {
                let ($($value,)+) = self;
                $($value.push_to_lua(lua)?;)+
                Ok($count)
            }
        }
    };
}

tuple_impl!(2; (A, a, 0), (B, b, 1));
tuple_impl!(3; (A, a, 0), (B, b, 1), (C, c, 2));
tuple_impl!(4; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3));
tuple_impl!(5; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4));
tuple_impl!(6; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5));
tuple_impl!(7; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6));
tuple_impl!(8; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6), (H, h, 7));
tuple_impl!(9; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6), (H, h, 7), (I, i, 8));
tuple_impl!(10; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6), (H, h, 7), (I, i, 8), (J, j, 9));
tuple_impl!(11; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6), (H, h, 7), (I, i, 8), (J, j, 9), (K, k, 10));
tuple_impl!(12; (A, a, 0), (B, b, 1), (C, c, 2), (D, d, 3), (E, e, 4), (F, f, 5), (G, g, 6), (H, h, 7), (I, i, 8), (J, j, 9), (K, k, 10), (L, l, 11));
