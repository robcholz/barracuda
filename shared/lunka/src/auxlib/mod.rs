use core::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr::{NonNull, null},
    slice::from_raw_parts,
};

use crate::{
    Managed, Thread,
    cdef::{auxlib::*, *},
};

mod buffer;
pub use buffer::*;
mod library;
pub use library::*;
mod options;
pub use options::*;

impl Thread {
    /// Raise an error reporting a problem with argument arg of the C function
    /// that called it, using a standard message that includes `extra_message`
    /// as a comment:
    ///
    /// `bad argument #<argument> to '<function name>' (<message>)`
    ///
    /// This function never returns.
    pub fn arg_error(&self, arg: c_int, extra_message: &CStr) -> ! {
        unsafe { luaL_argerror(self.as_ptr_no_gc(), arg, extra_message.as_ptr()) }
    }

    /// Check whether the function has an argument of any type (including `nil`)
    /// at position `arg`.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg`'s type is incorrect.
    pub fn check_any(&self, arg: c_int) {
        unsafe { luaL_checkany(self.as_ptr_no_gc(), arg) }
    }

    /// Check whether the function argument `arg` is an integer (or can be
    /// converted to an integer) and return this integer.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg`'s type is incorrect.
    pub fn check_integer(&self, arg: c_int) -> Integer {
        unsafe { luaL_checkinteger(self.as_ptr_no_gc(), arg) }
    }

    /// Check whether the function argument `arg` is a string and returns this
    /// string represented as a slice of [`c_char`]s.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string.
    pub fn check_c_chars(&self, arg: c_int) -> &[c_char] {
        let mut len = 0;
        let str_ptr = unsafe { luaL_checklstring(self.as_ptr_no_gc(), arg, &mut len as *mut _) };
        unsafe { from_raw_parts(str_ptr, len) }
    }

    /// Works the same as [`Thread::check_c_chars`], however it returns a slice
    /// of [`u8`]s instead of [`c_char`]s.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string.
    pub fn check_string(&self, arg: c_int) -> &[u8] {
        let mut len = 0;
        let str_ptr = unsafe { luaL_checklstring(self.as_ptr_no_gc(), arg, &mut len as *mut _) };
        unsafe { from_raw_parts(str_ptr as *const _, len) }
    }

    /// Check whether the function argument `arg` is a number and return this
    /// number converted to a [`Number`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg`'s type is incorrect.
    pub fn check_number(&self, arg: c_int) -> Number {
        unsafe { luaL_checknumber(self.as_ptr_no_gc(), arg) }
    }

    /// Check whether the function argument `arg` is a string, search for this
    /// string in the option list `list` and return the index in the list where
    /// the string was found.
    ///
    /// If `default` is `Some`, the function uses it as a default value when
    /// there is no argument `arg` or when this argument is `nil`.
    ///
    /// This is a useful function for mapping strings to C enums.
    /// (The usual convention in Lua libraries is to use strings instead of
    /// numbers to select options.)
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` is not a string or if the string cannot be found in `list`.
    pub fn check_option<const N: usize>(
        &self,
        arg: c_int,
        default: Option<&CStr>,
        list: &AuxOptions<'_, N>,
    ) -> usize {
        (unsafe {
            luaL_checkoption(
                self.as_ptr_no_gc(),
                arg,
                default.map(|cstr| cstr.as_ptr()).unwrap_or(null()),
                list.as_ptr(),
            )
        }) as _
    }

    /// Grow the stack size to `top + size` elements, raising an error if the
    /// stack cannot grow to that size.
    ///
    /// `message` is an additional text to go into the error message
    /// (or `None` for no additional text).
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// Lua stack cannot grow to the given size.
    pub fn check_stack(&self, size: c_int, message: Option<&CStr>) {
        unsafe {
            luaL_checkstack(
                self.as_ptr_no_gc(),
                size,
                message.map(|cstr| cstr.as_ptr()).unwrap_or(null()),
            )
        }
    }

    /// Check whether the function argument `arg` is a string and return this
    /// string represented by a [`CStr`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string.
    pub fn check_c_str(&self, arg: c_int) -> &CStr {
        let str_ptr = unsafe { luaL_checkstring(self.as_ptr_no_gc(), arg) };
        unsafe { CStr::from_ptr(str_ptr) }
    }

    /// Check whether the function argument `arg` has type `type_tag`.
    ///
    /// See also [`Type`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg`'s type is incorrect.
    pub fn check_type(&self, arg: c_int, type_tag: Type) {
        unsafe { luaL_checktype(self.as_ptr_no_gc(), arg, type_tag as _) }
    }

    /// Check whether the function argument `arg` is a userdata of the type
    /// `table_name` (see also [`Managed::new_metatable`]) and return the
    /// userdata's memory-block address (see [`Thread::to_userdata`]).
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg`'s type is incorrect.
    ///
    /// # Safety
    /// The returned pointer must only be used while it's valid.
    ///
    /// While the metatable of userdata is protected from modification in the Lua standard library,
    /// an unsound implementation of setting the metatable of an object in Lua could change a userdatum's metatable
    /// and make the check for the `table_name` metatable unsound.
    pub unsafe fn check_udata(&self, arg: c_int, table_name: &CStr) -> NonNull<c_void> {
        unsafe {
            NonNull::new_unchecked(luaL_checkudata(
                self.as_ptr_no_gc(),
                arg,
                table_name.as_ptr(),
            ))
        }
    }

    /// Check whether the code making the call and the Lua library being called
    /// are using the same version of Lua and the same numeric types.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// above requirements aren't met.
    pub fn check_version(&self) {
        unsafe { luaL_checkversion(self.as_ptr_no_gc()) }
    }

    /// Raise an error.
    ///
    /// This function adds the file name and the line number where the error
    /// occurred at the beginning of `message`, if this information is available.
    ///
    /// This function never returns.
    pub fn error_c_str(&self, message: &CStr) -> ! {
        unsafe { luaL_error(self.as_ptr_no_gc(), c"%s".as_ptr(), message.as_ptr()) }
    }

    /// Produce the return values for process-related functions in the standard
    /// library (`os.execute` and `io.close`).
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn exec_result(&self, status: c_int) -> c_int {
        unsafe { luaL_execresult(self.as_ptr_no_gc(), status) }
    }

    /// Produce the return values for file-related functions in the standard
    /// library (`io.open`, `os.rename`, `file:seek`, etc.).
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn file_result(&self, status: c_int, file_name: &CStr) -> c_int {
        unsafe { luaL_fileresult(self.as_ptr_no_gc(), status, file_name.as_ptr()) }
    }

    /// Push onto the stack the field `event` from the metatable of the object
    /// at index `obj_index` and return the type of the pushed value.
    ///
    /// If the object does not have a metatable, or if the metatable does not
    /// have this field, this function pushes nothing and returns [`Type::Nil`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn get_meta_field(&self, obj_index: c_int, event: &CStr) -> Type {
        unsafe {
            Type::from_c_int_unchecked(luaL_getmetafield(
                self.as_ptr_no_gc(),
                obj_index,
                event.as_ptr(),
            ))
        }
    }

    /// Push onto the stack the metatable associated with the name `table_name`
    /// in the registry (see also [`Managed::new_metatable`]), or `nil` if there
    /// is no metatable associated with that name, and return the type of the
    /// pushed value.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn get_aux_metatable(&self, table_name: &CStr) -> Type {
        unsafe {
            Type::from_c_int_unchecked(luaL_getmetatable(self.as_ptr_no_gc(), table_name.as_ptr()))
        }
    }

    /// If the function argument `arg` is an integer (or it is convertible to an
    /// integer), return this integer, or return `default`.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a number, isn't a `nil` and not absent.
    pub fn opt_integer(&self, arg: c_int, default: Integer) -> Integer {
        unsafe { luaL_optinteger(self.as_ptr_no_gc(), arg, default) }
    }

    /// If the function argument `arg` is a string, return this string, or
    /// return `default`.
    ///
    /// This function works like [`Thread::opt_string`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string, isn't a `nil` and not absent.
    pub fn opt_c_chars<'l>(&'l self, arg: c_int, default: &'l CStr) -> &'l [c_char] {
        let mut len = 0;
        let str_ptr = unsafe {
            luaL_optlstring(
                self.as_ptr_no_gc(),
                arg,
                default.as_ref().as_ptr(),
                &mut len as *mut _,
            )
        };
        unsafe { from_raw_parts(str_ptr, len) }
    }

    /// If the function argument `arg` is a string, return this string, or
    /// return `default`.
    ///
    /// This function works like [`Thread::opt_string`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string, isn't a `nil` and not absent.
    pub fn opt_c_str<'l>(&'l self, arg: c_int, default: &'l CStr) -> &'l CStr {
        unsafe { CStr::from_ptr(luaL_optstring(self.as_ptr_no_gc(), arg, default.as_ptr())) }
    }

    /// If the function argument `arg` is a string, return this string, or
    /// return `default`.
    ///
    /// This function uses [`Managed::to_string`] to get its result, so all
    /// conversions and caveats of that function apply here.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a string, isn't a `nil` and not absent.
    pub fn opt_string<'l>(&'l self, arg: c_int, default: &'l [u8]) -> &'l [u8] {
        let mut len = 0;
        let str_ptr = unsafe {
            luaL_optlstring(
                self.as_ptr_no_gc(),
                arg,
                default.as_ptr() as *const _,
                &mut len as *mut _,
            )
        };
        unsafe { from_raw_parts(str_ptr as *const _, len) }
    }

    /// If the function argument `arg` is a number, return this number as a
    /// [`Number`], or return `default`.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// argument `arg` isn't a number, isn't a `nil` and not absent.
    pub fn opt_number(&self, arg: c_int, default: Number) -> Number {
        unsafe { luaL_optnumber(self.as_ptr_inspect(), arg, default) }
    }

    /// Pushes the `fail` value onto the stack.
    pub fn push_fail(&self) {
        unsafe { luaL_pushfail(self.as_ptr_no_gc()) }
    }

    /// Set the metatable of the object on the top of the stack as the metatable
    /// associated with name `table_name` in the registry.
    ///
    /// See also [`Managed::new_metatable`].
    pub fn set_aux_metatable(&self, table_name: &CStr) {
        unsafe { luaL_setmetatable(self.as_ptr_no_gc(), table_name.as_ptr()) }
    }

    /// This function works like [`Thread::check_udata`], except that, when the
    /// test fails, it returns `None` instead of raising an error.
    ///
    /// # Safety
    /// The returned pointer must only be used while it's valid.
    ///
    /// While the metatable of userdata is protected from modification in the Lua standard library,
    /// an unsound implementation of setting the metatable of an object in Lua could change a userdatum's metatable
    /// and make the check for the `table_name` metatable unsound.
    pub unsafe fn test_udata(&self, arg: c_int, table_name: &CStr) -> Option<NonNull<c_void>> {
        NonNull::new(unsafe { luaL_testudata(self.as_ptr_inspect(), arg, table_name.as_ptr()) })
    }

    /// Raise a type error for the argument `arg` of the C function that called
    /// it, using a standard message;
    /// `type_name` is a "name" for the expected type.
    ///
    /// This function never returns.
    pub fn type_error(&self, arg: c_int, type_name: &CStr) -> ! {
        unsafe { luaL_typeerror(self.as_ptr_no_gc(), arg, type_name.as_ptr()) }
    }

    /// Return the name of the type of the value at the given index.
    pub fn type_name_of(&self, index: c_int) -> &CStr {
        unsafe { CStr::from_ptr(luaL_typename(self.as_ptr_no_gc(), index)) }
    }

    /// Release the reference `ref_idx` from the table at index `store_index`.
    ///
    /// If `ref_idx` is [`NO_REF`] or [`REF_NIL`], this function does nothing.
    ///
    /// The entry is removed from the table, so that the referred object can be
    /// collected.
    /// The reference `ref_idx` is also freed to be used again.
    ///
    /// See also [`Managed::create_ref`].
    pub fn destroy_ref(&self, store_index: c_int, ref_idx: c_int) {
        unsafe { luaL_unref(self.as_ptr_no_gc(), store_index, ref_idx) }
    }
}

impl Managed<'_> {
    /// Load a buffer as a Lua chunk.
    ///
    /// This function works like [`Managed::load_string`].
    pub fn load_c_chars(&mut self, buffer: &[c_char], name: &CStr) -> Status {
        unsafe {
            Status::from_c_int_unchecked(luaL_loadbuffer(
                self.as_ptr(),
                buffer.as_ptr(),
                buffer.len(),
                name.as_ptr(),
            ))
        }
    }

    /// Load a buffer as a Lua chunk.
    ///
    /// This function uses [`Managed::load`] to load the chunk in the buffer
    /// pointed to by `buffer`, and will return the same results as that
    /// function.
    ///
    /// `name` is the chunk name, used for debug information and error messages.
    // /// The string mode works as in the function lua_load.
    pub fn load_string<S: AsRef<[u8]>>(&mut self, buffer: S, name: &CStr) -> Status {
        let slice = buffer.as_ref();
        unsafe {
            Status::from_c_int_unchecked(luaL_loadbuffer(
                self.as_ptr(),
                slice.as_ptr() as *const _,
                slice.len(),
                name.as_ptr(),
            ))
        }
    }

    /// Load a file as a Lua chunk.
    ///
    /// This function uses [`Managed::load`] to load the chunk in the file
    /// `file_name`.
    ///
    /// The first line in the file is ignored if it starts with a #.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn load_file(&mut self, file_name: &CStr) -> Status {
        unsafe { Status::from_c_int_unchecked(luaL_loadfile(self.as_ptr(), file_name.as_ptr())) }
    }

    /// Load a Lua chunk from the standard input.
    ///
    /// This function uses [`Managed::load`] to load the chunk.
    ///
    /// The first line in the file is ignored if it starts with a `#`.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn load_stdin(&mut self) -> Status {
        unsafe { Status::from_c_int_unchecked(luaL_loadfile(self.as_ptr(), null())) }
    }

    /// Load a string as a Lua chunk.
    ///
    /// This function uses [`Managed::load`] to load `code`.
    pub fn load_c_str(&mut self, code: &CStr) -> Status {
        unsafe { Status::from_c_int_unchecked(luaL_loadstring(self.as_ptr(), code.as_ptr())) }
    }

    /// If the registry already doesn't have the key `table_name`, create a new
    /// table to be used as a metatable for userdata and return `true`.
    /// Otherwise, return `false`.
    ///
    /// In both cases, the function pushes onto the stack the final value
    /// associated with `table_name` in the registry.
    ///
    /// The function adds to this new table the pair `__name = table_name`,
    /// adds to the registry the pair `[table_name] = table`, and returns `true`.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn new_metatable(&mut self, table_name: &CStr) -> bool {
        (unsafe { luaL_newmetatable(self.as_ptr(), table_name.as_ptr()) }) != 0
    }

    /// Create and return a reference, in the table at index `store_index`, for
    /// the object on the top of the stack (popping the object).
    ///
    /// A reference is a unique integer key.
    /// As long as you do not manually add integer keys into the table
    /// `store_index`, this function ensures the uniqueness of the key it
    /// returns.
    ///
    /// You can retrieve an object referred by the reference `ref_idx` by
    /// calling [`thread.raw_get_i(store_index, ref_idx)`](Thread::raw_get_i).
    /// See also [`Thread::destroy_ref`], which frees a reference.
    ///
    /// If the object on the top of the stack is nil, this returns the constant
    /// [`REF_NIL`].
    /// The constant [`NO_REF`] is guaranteed to be different from any reference
    /// returned.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn create_ref(&mut self, store_index: c_int) -> c_int {
        unsafe { luaL_ref(self.as_ptr(), store_index) }
    }

    /// Create and push a traceback of the stack of thread `of`.
    ///
    /// If message is `Some`, it is appended at the beginning of the traceback.
    ///
    /// `level` tells at which level to start the traceback.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn traceback(&mut self, of: &Self, message: Option<&CStr>, level: c_int) {
        unsafe {
            luaL_traceback(
                self.as_ptr(),
                of.as_ptr_inspect(),
                message.map(|cstr| cstr.as_ptr()).unwrap_or(null()),
                level,
            )
        }
    }

    /// Create and push a traceback of the stack of this thread to its own stack.
    ///
    /// This function works like [`Managed::traceback`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn traceback_self(&mut self, message: Option<&CStr>, level: c_int) {
        unsafe {
            luaL_traceback(
                self.as_ptr(),
                self.as_ptr_inspect(),
                message.map(|cstr| cstr.as_ptr()).unwrap_or(null()),
                level,
            )
        }
    }

    /// Push onto the stack a string identifying the current position of the
    /// control at level `level` in the call stack.
    ///
    /// Typically, this string has the following format:
    ///
    /// `chunkname:currentline:`
    ///
    /// Level `0` is the running function, level `1` is the function that called
    /// the running function, etc.
    ///
    /// This function is used to build a prefix for error messages.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn where_string(&mut self, level: c_int) {
        unsafe { luaL_where(self.as_ptr(), level) }
    }
}
