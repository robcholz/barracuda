//! See [`Managed`].

use core::{ffi::c_void, ptr::null};

use crate::{AbsIndex, AcceptableIndex, Coroutine, Thread, ValidIndex, cdef::*};

#[cfg(feature = "auxlib")]
use crate::cdef::auxlib::*;

use core::{
    ffi::{CStr, c_char, c_int, c_uint},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::null_mut,
    slice::from_raw_parts,
};

/// Context for invalidating pointers that may be freed during garbage collection.
///
/// This structure is returned by [`Thread::managed`] and [`Thread::managed_no_gc`].
#[derive(Debug)]
#[repr(transparent)]
pub struct Managed<'l> {
    pub(crate) l: *mut lua_State,
    pub(crate) _life: PhantomData<&'l mut Thread>,
}

impl AsRef<Thread> for Managed<'_> {
    fn as_ref(&self) -> &Thread {
        unsafe { Thread::from_ptr(self.l) }
    }
}

impl AsMut<Thread> for Managed<'_> {
    fn as_mut(&mut self) -> &mut Thread {
        unsafe { Thread::from_ptr_mut(self.l) }
    }
}

impl Deref for Managed<'_> {
    type Target = Thread;
    fn deref(&self) -> &Self::Target {
        unsafe { Thread::from_ptr(self.l) }
    }
}

impl DerefMut for Managed<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { Thread::from_ptr_mut(self.l) }
    }
}

impl Managed<'_> {
    /// Perform an arithmetic or bitwise operation over two (or one) values at
    /// the top of the stack, popping them, and push the result of the operation.
    ///
    /// The value on the top is the second operand, and the value just below it
    /// is the first operand.
    ///
    /// This function follows the semantics of the corresponding Lua operator
    /// (that is, it may call metamethods).
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors)
    /// from a metamethod.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn arith(&mut self, operation: Arith) {
        unsafe { lua_arith(self.l, operation as _) }
    }

    /// Call a function (or a callable object).
    ///
    /// Like regular Lua calls, this function respects the `__call` metamethod.
    ///
    /// To do a call, you must use the following protocol:
    /// - First, the function to be called is pushed onto the stack.
    /// - Then, the arguments to the call are pushed in direct order; that is, the first argument is pushed first.
    /// - Finally, call [`Managed::call`]; `n_args` is the number of arguments that were pushed onto the stack.
    ///
    /// When the function returns, all arguments and the function value are
    /// popped and the call results are pushed onto the stack.
    /// The number of results is adjusted to `n_results`, unless it is
    /// [`MULT_RET`]. In this case, all results from the function are pushed;
    /// Lua takes care that the returned values fit into the stack space, but it
    /// does not ensure any extra space in the stack.
    /// The function results are pushed onto the stack in direct order (the
    /// first result is pushed first), so that after the call the last result is
    /// on the top of the stack.
    ///
    /// # Errors
    /// Any [error](crate::errors) while calling and running the function is
    /// propagated upwards (usually with a `longjmp`).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn call(&mut self, n_args: c_uint, n_results: c_int) {
        unsafe { lua_call(self.l, n_args as _, n_results) }
    }

    /// Behaves exactly like [`Managed::call`], but allows the called function
    /// to yield.
    ///
    /// If the callee yields, then, once the thread is resumed, the continuation
    /// function `continuation` will be called with the given context with
    /// exactly the same Lua stack as it was observed before the yield.
    ///
    /// # Errors
    /// Any [error](crate::errors) while calling and running the function is
    /// propagated upwards (usually with a `longjmp`).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn call_k(
        &mut self,
        n_args: c_uint,
        n_results: c_int,
        continuation: lua_KFunction,
        context: lua_KContext,
    ) {
        unsafe { lua_callk(self.l, n_args as _, n_results, context, Some(continuation)) }
    }

    /// Close the to-be-closed slot at the given index and set its value to `nil`.
    ///
    /// A `__close` metamethod cannot yield when called through this function.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors)
    /// from the `__close` metamethod.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    ///
    /// The index must be the last index previously marked to be closed that is
    /// still active (that is, not closed yet).
    pub unsafe fn close_slot(&mut self, index: c_int) {
        unsafe { lua_closeslot(self.l, index) }
    }

    /// Compare two Lua values.
    ///
    /// Returns `true` if the value at `idx_a` satisfies `operation` when
    /// compared with the value at index `idx_b`, following the semantics of the
    /// corresponding Lua operator (that is, it may call metamethods).
    /// Otherwise returns `false`.
    /// Also returns `false` if any of the indices are not valid.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors)
    /// from a metamethod.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn compare(&mut self, operation: Compare, idx_a: c_int, idx_b: c_int) -> bool {
        (unsafe { lua_compare(self.l, idx_a, idx_b, operation as _) }) != 0
    }

    /// Concatenate `n` values at the top of the stack, popping them, and leave
    /// the result on the top.
    ///
    /// If `n` is `1`, the result is the single value on the stack (that is, the
    /// function does nothing).
    ///
    /// If `n` is `0`, the result is an empty string.
    ///
    /// Concatenation is performed following the usual semantics of Lua.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors)
    /// from a metamethod.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn concat(&mut self, n: c_uint) {
        unsafe { lua_concat(self.l, n as _) }
    }

    /// Create a new empty table and push it onto the stack.
    ///
    /// `narr` is a hint for how many elements the table will have as a sequence,
    /// and `nrec` is a hint for how many other elements the table will have.
    /// Lua may use these hints to preallocate memory for the new table.
    /// This preallocation may help performance when its known in advance how
    /// many elements the table will have.
    ///
    /// See also [`Managed::new_table`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn create_table(&mut self, n_arr: c_uint, n_rec: c_uint) {
        unsafe { lua_createtable(self.as_ptr(), n_arr as _, n_rec as _) }
    }

    /// Dump a function as a binary chunk, and return the status of the
    /// operation.
    ///
    /// This function receives a Lua function on the top of the stack and
    /// produces a binary chunk that, if loaded again, results in a function
    /// equivalent to the one dumped.
    ///
    /// As it produces parts of the chunk, the function calls `writer` (see also
    /// [`lua_Writer`]) with the given data to write them.
    /// If `strip_debug_info` is `true`, the binary representation may not
    /// include all debug information about the function, to save space.
    ///
    /// The value returned is the error code returned by the last call to the
    /// writer.
    ///
    /// This function does not pop the Lua function from the stack.
    ///
    /// # Safety
    /// `writer_data` must be valid to be passed to `writer`.
    pub unsafe fn dump(
        &mut self,
        writer: lua_Writer,
        writer_data: *mut c_void,
        strip_debug_info: bool,
    ) -> c_int {
        unsafe {
            lua_dump(
                self.as_ptr(),
                writer,
                writer_data,
                if strip_debug_info { 1 } else { 0 },
            )
        }
    }

    /// Push onto the stack the value of the global `name`, and return the type
    /// of that value.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    pub fn get_global(&mut self, name: &CStr) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_getglobal(self.as_ptr(), name.as_ptr())) }
    }

    /// Load a Lua chunk without running it.
    ///
    /// If there are no errors, push the compiled chunk as a Lua function.
    /// Otherwise, push an error message.
    ///
    /// This function uses a user-supplied `reader` to read the chunk
    /// (see also [`lua_Reader`]).
    /// `reader_data` is an opaque value passed to the reader function.
    ///
    /// `chunk_name` gives a name to the chunk, which is used for error messages
    /// and in debug information.
    ///
    /// The function automatically detects whether the chunk is text or binary
    /// and loads it accordingly.
    /// The string `mode` works similarly as in the Lua base library function
    /// `load`:
    /// - `Some("b")` loads only binary chunks.
    /// - `Some("t")` loads only text chunks.
    /// - `Some("bt")` loads both binary and text chunks.
    /// - `None` is equivalent to the string `"bt"`.
    ///
    /// This function uses the stack internally, so `reader` must always leave
    /// the stack *unmodified* when returning.
    ///
    /// If the resulting function has upvalues, its first upvalue is set to the
    /// value of the global environment stored at index [`REGISTRY_GLOBALS`] in
    /// the registry.
    /// When loading main chunks, this upvalue will be the `_ENV` variable.
    /// Other upvalues are initialized with `nil`.
    ///
    /// # Safety
    /// `reader_data` must be valid to be passed to `reader`.
    pub unsafe fn load(
        &mut self,
        reader: lua_Reader,
        reader_data: *mut c_void,
        chunk_name: &CStr,
        mode: Option<&CStr>,
    ) -> Status {
        unsafe {
            Status::from_c_int_unchecked(lua_load(
                self.as_ptr(),
                reader,
                reader_data,
                chunk_name.as_ptr(),
                mode.map(|cstr| cstr.as_ptr()).unwrap_or(null()),
            ))
        }
    }

    /// Create a new empty table and push it onto the stack.
    ///
    /// See also [`Managed::create_table`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn new_table(&mut self) {
        unsafe { lua_newtable(self.as_ptr()) }
    }

    /// Create a new thread, push it on the stack, and return a [`Coroutine`]
    /// that represents this new thread.
    ///
    /// The new thread returned by this function shares with the original thread
    /// its global environment, but has an independent execution stack.
    /// Threads are subject to garbage collection, like any Lua object.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn new_thread(&mut self) -> Coroutine<'_> {
        Coroutine::new(unsafe { Thread::from_ptr_mut(lua_newthread(self.as_ptr())) })
    }

    /// Create and push on the stack a new full userdata, with `n_uservalues`
    /// associated Lua values, called user values, and an associated block of
    /// raw memory of `size` bytes.
    ///
    /// The function returns a pointer to the block of memory that was allocated
    /// by Lua.
    ///
    /// The user values can be set and read with the functions
    /// [`Thread::set_i_uservalue`] and [`Thread::get_i_uservalue`].
    ///
    /// You may use this function if, for instance, the layout of the data in
    /// the allocation changes based on run-time information.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// Lua ensures that the pointer is valid as long as the corresponding userdata is alive.
    /// Moreover, if the userdata is marked for finalization,
    /// it is valid at least until the call to its finalizer.
    /// The returned pointer must only be used while it's valid.
    ///
    /// Lua makes no guarantees about the alignment of the pointer -
    /// it depends entirely on the allocator function used.
    /// However, generally, the maximum alignment for
    /// a type that can be written is [`MAX_ALIGN`].
    pub unsafe fn new_userdata_raw(&mut self, size: usize, n_uservalues: c_int) -> *mut c_void {
        unsafe { lua_newuserdatauv(self.as_ptr(), size, n_uservalues) }
    }

    /// Push a new C closure onto the stack.
    ///
    /// This function receives a C function `func` and pushes onto the stack a
    /// Lua value of type `function` that, when called, invokes the
    /// corresponding C function.
    /// `n_upvalues` tells how many upvalues this function will have.
    ///
    /// Any function to be callable by Lua must follow the correct protocol to
    /// receive its parameters and return its results (see [`lua_CFunction`]).
    ///
    /// # C closures
    /// When a C function is created, it is possible to associate some values
    /// with it, which are called *upvalues*.
    /// These upvalues are then accessible to the function whenever it is called,
    /// where the function is called a *C closure*. To create a C closure:
    /// 1. Push the initial values for its upvalues onto the stack.
    ///    (When there are multiple upvalues, the first value is pushed first.)
    /// 2. Call this function with the argument `n_upvalues`
    ///    telling how many upvalues will be associated with the function.
    ///    The function will also pop these values from the stack.
    ///
    /// When `n_upvalues == 0`, this function creates a "light" C function,
    /// which is just a pointer to the C function. In that case, it never raises
    /// a memory error.
    ///
    /// See also [`Thread::push_c_function`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors) if `n_upvalues > 0`.
    pub fn push_c_closure(&mut self, func: lua_CFunction, n_upvalues: c_int) {
        unsafe { lua_pushcclosure(self.as_ptr(), func, n_upvalues) }
    }

    /// Works the same as [`Managed::push_string`], however it accepts
    /// [`c_char`]s instead of [`u8`]s.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn push_c_chars<'l>(&'l mut self, data: &[c_char]) -> &'l [c_char] {
        let length = data.len();
        unsafe {
            from_raw_parts(
                lua_pushlstring(self.as_ptr(), data.as_ptr(), length),
                length,
            )
        }
    }

    /// Push a string onto the stack.
    ///
    /// The string can contain any binary data, including embedded zeros.
    ///
    /// Lua will make or reuse an internal copy of the given string, so the
    /// memory pointed to by `data` can be safely freed or reused immediately
    /// after the function returns.
    ///
    /// See also [`Managed::push_c_chars`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn push_string<S: AsRef<[u8]>>(&mut self, data: S) -> &[u8] {
        let slice = data.as_ref();
        let length = slice.len();
        unsafe {
            from_raw_parts(
                lua_pushlstring(self.as_ptr(), slice.as_ptr() as *const _, length) as *const _,
                length,
            )
        }
    }

    /// Push a zero-terminated string onto the stack.
    ///
    /// Lua will make or reuse an internal copy of the given string, so the
    /// memory pointed to by `data` can be freed or reused immediately after the
    /// function returns.
    ///
    /// See also [`Managed::push_c_chars`] and [`Managed::push_string`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn push_c_str<'l>(&'l mut self, data: &CStr) -> &'l CStr {
        unsafe { CStr::from_ptr(lua_pushstring(self.as_ptr(), data.as_ptr())) }
    }

    /// Without metamethods, do `t[k] = v`, where `t` is the value at the given
    /// index, `v` is the value on the top of the stack, and `k` is the value
    /// just below the top.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_set(&mut self, tbl_index: AcceptableIndex) {
        unsafe { lua_rawset(self.as_ptr(), tbl_index) }
    }

    /// Without metamethods, do `t[i] = v`, where `t` is the value at the given
    /// index and `v` is the value on the top of the stack.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_set_i(&mut self, tbl_index: AcceptableIndex, i: Integer) {
        unsafe { lua_rawseti(self.as_ptr(), tbl_index, i) }
    }

    /// Without metamethods, do `t[ptr] = v`, where `t` is the value at the
    /// given index, `v` is the value on the top of the stack, and `ptr` is the
    /// given pointer represented as a light userdata.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_set_p(&mut self, tbl_index: AcceptableIndex, ptr: *const c_void) {
        unsafe { lua_rawsetp(self.as_ptr(), tbl_index, ptr) }
    }

    /// Remove the element at the given valid index, shifting down the elements
    /// above this index to fill the gap.
    ///
    /// This function cannot be called with a pseudo-index, because a
    /// pseudo-index is not an actual stack position.
    pub fn remove(&mut self, index: AbsIndex) {
        unsafe { lua_remove(self.as_ptr(), index.get()) }
    }

    /// Perform a full garbage collection cycle.
    pub fn run_gc(&mut self) {
        unsafe { lua_gc(self.l, GcTask::Collect as _) };
    }

    /// Pop a value from the stack and set it as the new value of global `name`.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    pub fn set_global(&mut self, key: &CStr) {
        unsafe { lua_setglobal(self.as_ptr(), key.as_ptr()) }
    }

    /// Perform an incremental step of garbage collection, corresponding to the
    /// allocation of `stepsize` kilobytes.
    pub fn step_gc(&mut self, step_size: c_uint) {
        unsafe { lua_gc(self.l, GcTask::Step as _, step_size) };
    }

    /// Convert the Lua value at the given index to a slice of [`c_char`]s,
    /// representing a Lua string.
    ///
    /// This function works like [`Managed::to_string`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn to_c_chars(&mut self, index: c_int) -> Option<&[c_char]> {
        let mut len = 0;
        let str_ptr = unsafe { lua_tolstring(self.as_ptr(), index, &mut len as *mut _) };
        if !str_ptr.is_null() {
            Some(unsafe { from_raw_parts(str_ptr, len) })
        } else {
            None
        }
    }

    /// Convert the Lua value at the given index to a [`CStr`],
    /// representing a Lua string.
    ///
    /// This function works like [`Managed::to_string`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn to_c_str(&mut self, index: c_int) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(lua_tostring(self.as_ptr(), index)) }
    }

    /// Convert the Lua value at the given index to a slice of [`u8`]s,
    /// representing a Lua string.
    ///
    /// The Lua value must be a string or a number; otherwise, the function
    /// returns `None`.
    ///
    /// If the value is a number, then this function also changes the *actual
    /// value in the stack* to a string.
    ///
    /// The function returns a slice to data inside the Lua state.
    /// This string always has a zero (`'\0'`) after its last character (as in C),
    /// but can contain other zeros in its body.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn to_string(&mut self, index: c_int) -> Option<&[u8]> {
        let mut len = 0;
        let str_ptr = unsafe { lua_tolstring(self.as_ptr(), index, &mut len as *mut _) };
        if !str_ptr.is_null() {
            Some(unsafe { from_raw_parts(str_ptr as *const _, len) })
        } else {
            None
        }
    }

    /// Push onto the stack the value `t[key]`, where `t` is the value at the
    /// given index, and return the type of the pushed value.
    ///
    /// As in Lua, this function may trigger a metamethod for the `index` event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn get_field(&mut self, obj_index: c_int, key: &CStr) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_getfield(self.l, obj_index, key.as_ptr())) }
    }

    /// Push onto the stack the value `t[i]`, where `t` is the value at the
    /// given index, and return the type of the pushed value.
    ///
    /// As in Lua, this function may trigger a metamethod for the `index` event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn get_i(&mut self, obj_index: c_int, i: Integer) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_geti(self.l, obj_index, i)) }
    }

    /// Push onto the stack the value `t[k]`, where `t` is the value at the
    /// given index and `k` is the value on the top of the stack, and return the
    /// type of the pushed value.
    ///
    /// This function pops the key from the stack, pushing the resulting value
    /// in its place.
    ///
    /// As in Lua, this function may trigger a metamethod for the `index` event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn get_table(&mut self, obj_index: c_int) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_gettable(self.l, obj_index)) }
    }

    /// Push onto the stack the length of the value at the given index.
    ///
    /// This function is equivalent to the `#` operator in Lua and may trigger a
    /// metamethod for the `length` event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn length_of(&mut self, index: c_int) {
        unsafe { lua_len(self.l, index) }
    }

    /// Start or resume this thread (like a coroutine).
    ///
    /// See also [`Managed::resume_from`].
    ///
    /// This function returns [`Status::Yielded`] if the coroutine yields,
    /// [`Status::Ok`] if the coroutine finishes its execution without errors,
    /// or an error code in case of errors.
    /// In case of errors, the error object is on the top of the stack.
    ///
    /// ## Starting a coroutine
    /// To start a coroutine:
    /// - Push the main function plus any arguments onto the empty stack of the thread.
    /// - Then, call this function, with `n_args` being the number of arguments.
    ///   This call returns when the coroutine suspends or finishes its execution.
    ///
    /// When it returns, the number of results is saved and the top of the stack
    /// contains the values passed to [`Thread::yield_with`] or returned by the
    /// body function.
    ///
    /// ## Resuming a coroutine
    /// To resume a coroutine:
    /// - Remove the yielded values from its stack.
    /// - Push the values to be passed as results from the yield.
    /// - Call this function.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn resume(&mut self, n_args: c_uint) -> (Status, c_int) {
        let mut n_res = 0;
        let status =
            unsafe { lua_resume(self.as_ptr(), null_mut(), n_args as _, &mut n_res as *mut _) };
        (unsafe { Status::from_c_int_unchecked(status) }, n_res)
    }

    /// Resume this thread, also specifying the thread that is resuming this one.
    ///
    /// See [`Managed::resume`] for more information.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn resume_from(&mut self, from: &mut Self, n_args: c_uint) -> (Status, c_int) {
        let mut n_res = 0;
        let status = unsafe {
            lua_resume(
                self.as_ptr(),
                from.as_ptr(),
                n_args as _,
                &mut n_res as *mut _,
            )
        };
        (unsafe { Status::from_c_int_unchecked(status) }, n_res)
    }

    /// Call a function (or a callable object) in protected mode.
    ///
    /// Both `n_args` and `n_results` have the same meaning as in
    /// [`Managed::call`].
    ///
    /// If there are no errors during the call, this function behaves exactly
    /// like [`Managed::call`].
    /// However, if there is any error, the function catches it, pushes a single
    /// value on the stack (the error object), and returns an error code.
    /// Like [`Managed::call`], this function always removes the function and
    /// its arguments from the stack.
    ///
    /// If `err_func` is `0`, then the error object returned on the stack is
    /// exactly the original error object.
    /// Otherwise, `err_func` is the stack index of a message handler.
    /// (This index cannot be a pseudo-index.)
    /// In case of runtime errors, this handler will be called with the error
    /// object and its return value will be the object returned on the stack.
    ///
    /// Typically, the message handler is used to add more debug information to
    /// the error object, such as a stack traceback.
    /// Such information cannot be gathered after the return of this function,
    /// since by then the stack has unwound.
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn pcall(&mut self, n_args: c_uint, n_results: c_int, err_func: c_int) -> Status {
        unsafe { Status::from_c_int_unchecked(lua_pcall(self.l, n_args as _, n_results, err_func)) }
    }

    /// Behaves exactly like [`Managed::pcall`], but allows the called function
    /// to yield.
    ///
    /// If the callee yields, then, once the thread is resumed, the continuation
    /// function `continuation` will be called with the given context with
    /// exactly the same Lua stack as it was observed before the yield.
    ///
    /// # Errors
    /// Yielding will cause a jump similar to an error (usually, with a `longjmp`).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn pcall_k(
        &mut self,
        n_args: c_uint,
        n_results: c_int,
        err_func: c_int,
        continuation: lua_KFunction,
        context: lua_KContext,
    ) -> Status {
        unsafe {
            Status::from_c_int_unchecked(lua_pcallk(
                self.l,
                n_args as _,
                n_results,
                err_func,
                context,
                Some(continuation),
            ))
        }
    }

    /// Pop `n` elements from the stack.
    ///
    /// Because this is implemented with [`Managed::set_top`], this function can
    /// also run arbitrary code when removing an index marked as to-be-closed
    /// from the stack.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn pop(&mut self, n: c_uint) {
        unsafe { lua_pop(self.l, n as _) }
    }

    /// Set the C function `func` as the new value of global `name`,
    /// calling metamethods for the global table, if there are any.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn register(&mut self, name: &CStr, func: lua_CFunction) {
        unsafe { lua_register(self.as_ptr(), name.as_ptr(), func) }
    }

    /// If `package.loaded[modname]` is not true, calls the function `open_fn`
    /// with the string `module_name` as an argument and sets the call result to
    /// `package.loaded[modname]`, as if that function has been called through
    /// `require`.
    ///
    /// This function leaves a copy of the module on the stack.
    ///
    /// If `into_global` is true, also stores the module into the global
    /// `module_name`.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn require(
        &mut self,
        module_name: &CStr,
        open_fn: lua_CFunction,
        into_global: bool,
    ) {
        unsafe {
            luaL_requiref(
                self.l,
                module_name.as_ptr(),
                open_fn,
                if into_global { 1 } else { 0 },
            )
        }
    }

    /// Do the equivalent to `t[key] = v`, where `t` is the value at the given
    /// index and `v` is the value on the top of the stack.
    ///
    /// This function pops the value from the stack.
    ///
    /// As in Lua, this function may trigger a metamethod for the `newindex`
    /// event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn set_field(&mut self, obj_index: c_int, key: &CStr) {
        unsafe { lua_setfield(self.l, obj_index, key.as_ptr()) }
    }

    /// Do the equivalent to `t[k] = v`, where `t` is the value at the given
    /// index, `v` is the value on the top of the stack, and `k` is the value
    /// just below the top.
    ///
    /// This function pops the value from the stack.
    ///
    /// As in Lua, this function may trigger a metamethod for the `newindex`
    /// event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn set_table(&mut self, obj_index: c_int) {
        unsafe { lua_settable(self.l, obj_index) }
    }

    /// Accept any index, or `0`, and set the stack top to this index.
    ///
    /// If the new top is greater than the old one, then the new elements are
    /// filled with `nil`. If index is 0, then all stack elements are removed.
    ///
    /// This function can run arbitrary code when removing an index marked as
    /// to-be-closed from the stack.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn set_top(&mut self, top: c_int) {
        unsafe { lua_settop(self.l, top) }
    }

    /// Do the equivalent to `t[i] = v`, where `t` is the value at the given
    /// index and `v` is the value on the top of the stack.
    ///
    /// This function pops the value from the stack.
    ///
    /// As in Lua, this function may trigger a metamethod for the `newindex`
    /// event.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn set_i(&mut self, obj_index: c_int, i: Integer) {
        unsafe { lua_seti(self.l, obj_index, i) }
    }
}

#[cfg(feature = "stdlibs")]
impl Managed<'_> {
    /// Open all standard Lua libraries into the associated Lua thread.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    pub fn open_libs(&mut self) {
        unsafe { stdlibs::luaL_openlibs(self.l) }
    }
}

#[cfg(feature = "auxlib")]
impl<'l> Managed<'l> {
    /// Call a metamethod `event` on the object at index `obj_index`.
    ///
    /// If the object at index `obj_index` has a metatable and this metatable
    /// has a field `event`, this function calls this field passing the object
    /// as its only argument.
    /// In this case, this function returns `true` and pushes onto the stack the
    /// value returned by the call.
    ///
    /// If there is no metatable or no metamethod, this function returns `false`
    /// without pushing any value on the stack.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn call_metamethod(&mut self, obj_index: c_int, event: &CStr) -> bool {
        (unsafe { luaL_callmeta(self.l, obj_index, event.as_ptr()) }) != 0
    }

    /// Load and run the given file.
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    pub fn do_file(&mut self, file_name: &CStr) -> bool {
        unsafe { luaL_dofile(self.l, file_name.as_ptr()) }
    }

    /// Load and run the given string.
    pub fn do_string(&mut self, code: &CStr) -> bool {
        unsafe { luaL_dostring(self.l, code.as_ptr()) }
    }

    /// Ensure that the value `t[table_name]`, where `t` is the value at index
    /// `parent_index`, is a table, and push that table onto the stack.
    ///
    /// This function returns `true` if it finds a previous table there and
    /// `false` if it creates a new table.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn get_sub_table(&mut self, parent_index: c_int, table_name: &CStr) -> bool {
        (unsafe { luaL_getsubtable(self.l, parent_index, table_name.as_ptr()) }) != 0
    }

    /// Return the "length" of the value at the given index as a number.
    ///
    /// This function is equivalent to the `#` operator in Lua.
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors) if the
    /// result of the operation is not an integer.
    /// (This case can only happen through metamethods.)
    ///
    /// It can also raise an error from a metamethod.
    pub fn meta_length_of(&mut self, obj_index: c_int) -> Integer {
        unsafe { luaL_len(self.l, obj_index) }
    }

    /// Convert any Lua value at the given index to a string in a reasonable
    /// format, and push that string onto the stack.
    ///
    /// This function works the same way as [`Managed::to_string_meta`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn to_c_chars_meta(&mut self, index: c_int) -> &'l [c_char] {
        let mut len = 0;
        let str_ptr = unsafe { luaL_tolstring(self.l, index, &mut len as *mut _) };
        unsafe { from_raw_parts(str_ptr, len) }
    }

    /// Convert any Lua value at the given index to a string in a reasonable
    /// format, and push that string onto the stack.
    ///
    /// This function works the same way as [`Managed::to_string_meta`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn to_c_str_meta(&mut self, index: c_int) -> &'l CStr {
        let str_ptr = unsafe { luaL_tolstring(self.l, index, null_mut()) };
        unsafe { CStr::from_ptr(str_ptr) }
    }

    /// Convert any Lua value at the given index to a string in a reasonable
    /// format, and push that string onto the stack.
    ///
    /// The string created by the function is represented as the return value.
    ///
    /// If the value has a metatable with a `__tostring` field, then this
    /// function calls the corresponding metamethod with the value as argument,
    /// and uses the result of the call as its result.
    ///
    /// # Errors
    /// The underlying Lua state may raise an arbitrary [error](crate::errors).
    ///
    /// # Safety
    /// Calling untrusted code in a possibly-unsound environment can cause Undefined Behavior.
    pub unsafe fn to_string_meta(&mut self, index: c_int) -> &'l [u8] {
        let mut len = 0;
        let str_ptr = unsafe { luaL_tolstring(self.l, index, &mut len as *mut _) };
        unsafe { from_raw_parts(str_ptr as *const _, len) }
    }
}
