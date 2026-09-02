use core::{
    ffi::{CStr, c_char},
    fmt::{self, Write},
    marker::PhantomData,
    mem::MaybeUninit,
};

use crate::{Managed, Thread, cdef::auxlib::*};

/// String buffer that allows code to build Lua strings piecemeal.
#[derive(Debug)]
#[repr(transparent)]
pub struct Buffer<'l> {
    raw: luaL_Buffer,
    thread: PhantomData<&'l mut Thread>,
}

impl<'l> Buffer<'l> {
    const unsafe fn from_raw(raw: luaL_Buffer) -> Self {
        Self {
            raw,
            thread: PhantomData,
        }
    }

    fn new(lua: &'l mut Thread) -> Self {
        let raw = unsafe {
            let mut raw = MaybeUninit::<luaL_Buffer>::zeroed();
            luaL_buffinit(lua.as_ptr(), raw.as_mut_ptr());
            raw.assume_init()
        };
        unsafe { Self::from_raw(raw) }
    }

    fn with_capacity(lua: &'l mut Thread, capacity: usize) -> Self {
        let raw = unsafe {
            let mut raw = MaybeUninit::<luaL_Buffer>::zeroed();
            luaL_buffinitsize(lua.as_ptr(), raw.as_mut_ptr(), capacity);
            raw.assume_init()
        };
        unsafe { Self::from_raw(raw) }
    }

    fn finish(mut self) {
        debug_assert!(
            self.raw.n <= self.raw.size,
            "buffer length is bigger than its capacity"
        );
        unsafe { luaL_pushresult(&mut self.raw) }
    }

    /// Allocate enough space in the buffer for a given number of C characters,
    /// and return a pointer to it.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// The returned pointer may only be used while the buffer is valid.
    pub unsafe fn prep(&mut self, size: usize) -> *mut c_char {
        unsafe { luaL_prepbuffsize(&mut self.raw, size) }
    }

    /// Allocate enough space in the buffer for [`BUFFER_SIZE`] C characters,
    /// and return a pointer to it.
    ///
    /// See also [`prep`](Self::prep).
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// The returned pointer may only be used while the buffer is valid.
    pub unsafe fn prep_default(&mut self) -> *mut c_char {
        unsafe { self.prep(BUFFER_SIZE) }
    }

    /// Get the current length of the buffer.
    ///
    /// Equivalent to the C macro `luaL_bufflen`.
    pub const fn len(&self) -> usize {
        self.raw.n
    }

    /// Set the current length of the buffer.
    ///
    /// # Safety
    /// `len` must be less than or equal to the reported capacity,
    /// and all bytes in range should be initialized.
    pub const unsafe fn set_len(&mut self, len: usize) {
        self.raw.n = len
    }

    /// Return `true` if the buffer is empty.
    pub const fn is_empty(&self) -> bool {
        self.raw.n == 0
    }

    /// Get the current capacity of the buffer.
    pub const fn capacity(&self) -> usize {
        self.raw.size
    }

    /// Returns an immutable reference to the [`Thread`].
    pub fn thread(&self) -> &'l Thread {
        unsafe { Thread::from_ptr(self.raw.L) }
    }

    /// Returns a mutable reference to the [`Thread`].
    pub fn thread_mut(&mut self) -> &'l mut Thread {
        unsafe { Thread::from_ptr_mut(self.raw.L) }
    }

    /// Add one C character to the buffer.
    ///
    /// Equivalent to the C macro `luaL_addchar`.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn add_char(&mut self, ch: c_char) {
        let len = self.len();
        if len >= self.capacity() {
            unsafe { luaL_prepbuffsize(&mut self.raw, 1) };
        }
        unsafe { *self.raw.b.add(len) = ch }
        unsafe { self.set_len(len.wrapping_add(1)) }
    }

    /// Add one byte to the buffer.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn add_byte(&mut self, b: u8) {
        self.add_char(b as _)
    }

    /// Remove a given number of C characters from the buffer.
    ///
    /// Equivalent to the C macro `luaL_buffsub`.
    pub fn remove(&mut self, n: usize) {
        unsafe { self.set_len(self.len().saturating_sub(n)) }
    }

    /// Add a string to the buffer.
    ///
    /// Functionally equivalent to the function [`luaL_addlstring`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn add_string<S: AsRef<[u8]>>(&mut self, data: S) {
        let data = data.as_ref();
        unsafe { luaL_addlstring(&mut self.raw, data.as_ptr() as *const _, data.len()) }
    }

    /// Add a C string to the buffer.
    ///
    /// Equivalent to the function [`luaL_addstring`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn add_c_str(&mut self, data: &CStr) {
        unsafe { luaL_addstring(&mut self.raw, data.as_ptr()) }
    }

    /// Convert a value on top of the associated Lua stack to a string, and pop
    /// the result into the buffer.
    ///
    /// Equivalent to the function [`luaL_addvalue`].
    pub fn add_value(&mut self) {
        unsafe { luaL_addvalue(&mut self.raw) }
    }
}

impl fmt::Write for Buffer<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.add_string(s.as_bytes());
        Ok(())
    }

    fn write_char(&mut self, c: char) -> fmt::Result {
        let mut char_data = [0u8; 4];
        c.encode_utf8(&mut char_data);
        self.add_string(char_data);
        Ok(())
    }
}

impl Managed<'_> {
    /// Fill a [`Buffer`]
    /// with the given closure
    /// and push it onto the stack.
    pub fn push_buffer<F: FnOnce(&mut Buffer<'_>)>(&mut self, f: F) {
        let mut buffer = Buffer::new(self);
        f(&mut buffer);
        buffer.finish()
    }

    /// Fill a [`Buffer`] of a known capacity
    /// with the given closure
    /// and push it onto the stack.
    pub fn push_buffer_with_capacity<F: FnOnce(&mut Buffer<'_>)>(&mut self, capacity: usize, f: F) {
        let mut buffer = Buffer::with_capacity(self, capacity);
        f(&mut buffer);
        buffer.finish()
    }

    /// Pushes a [`Display`](fmt::Display) representation of `T` onto the stack
    /// as a Lua string.
    pub fn push_display<T: ?Sized + fmt::Display>(&mut self, t: &T) {
        self.push_buffer(move |buffer| {
            let _ = write!(buffer, "{t}");
        })
    }

    /// Pushes a [`Debug`](fmt::Debug) representation of `T` onto the stack
    /// as a Lua string.
    pub fn push_debug<T: ?Sized + fmt::Debug>(&mut self, t: &T) {
        self.push_buffer(move |buffer| {
            let _ = write!(buffer, "{t:?}");
        })
    }
}
