//! See [`Thread`].

use crate::{AbsIndex, AcceptableIndex, GcMode, ValidIndex, cdef::*, managed::*};

use core::{
    cell::UnsafeCell,
    ffi::{CStr, c_int, c_uint, c_void},
    marker::PhantomData,
    ptr::null_mut,
};

macro_rules! lua_is {
	(
		@bool
		$(#[$attr:meta])*
		$vis:vis fn $name:ident(&self, index: AcceptableIndex) -> bool
		for $ffi_fn:ident
	) => {
		$(#[$attr])*
		$vis fn $name(&self, index: crate::AcceptableIndex) -> bool {
			unsafe { $ffi_fn(self.as_ptr_inspect(), index) }
		}
	};

	(
		@c_int
		$(#[$attr:meta])*
		$vis:vis fn $name:ident(&self, index: AcceptableIndex) -> bool
		for $ffi_fn:ident
	) => {
		$(#[$attr])*
		$vis fn $name(&self, index: crate::AcceptableIndex) -> bool {
			(unsafe { $ffi_fn(self.as_ptr_inspect(), index) }) != 0
		}
	};

	(
		$(
			$(#[$attr:meta])*
			$vis:vis fn $name:ident(&self, index: AcceptableIndex) -> bool
			for $ffi_fn:ident -> $ffi_fn_ret:tt;
		)*
	) => {
		$(
			lua_is!{
				@ $ffi_fn_ret
				$(#[$attr])*
				$vis fn $name(&self, index: AcceptableIndex) -> bool
				for $ffi_fn
			}
		)*
	};
}

/// Opaque type that represents a Lua thread, which is used by
/// [`Lua`](crate::Lua) and other structures.
///
/// This type can never have an instance made of it; there can only be
/// references to this type, which are the only kind of valid value.
///
/// # Borrowing
/// Most methods use `&mut Thread` as `&Thread`, even though, at least
/// *in theory*, they should've borrowed mutably with `&mut Thread`.
/// For instance, [`Thread::push_nil`], even though it modifies the Lua stack by
/// pushing a `nil`, still borrows a [`Thread`] immutably.
///
/// In the case for this structure, borrowing immutably means **not in any way
/// being able to trigger the GC to collect garbage**.
/// Any references returned by methods for this structure are simply the same
/// pointers that the C API returns, though they are converted to references for
/// the Rust borrow checker to ensure safety.
/// The Lua garbage collector will not invalidate any pointers if it is stopped.
///
/// To call API functions that can potentially enable the GC, it is required
/// that any references that have been acquired previously from a [`Thread`] are
/// immediately invalidated, so they cannot be used *if* the garbage collector
/// decides to collect them.
/// This is done by borrowing [`Thread`] mutably once, through
/// [`Thread::managed`], which allows for more operations.
///
/// The main reason for this model existing is because it may be difficult to
/// formally prove that a reference would not be collected without using stack
/// indices. This model simply utilizes checks done at compile time to ensure
/// safety.
///
/// # Layout
/// This type has the same layout and ABI as [`lua_State`];
/// however, it is always used behind a reference.
#[derive(Debug)]
#[repr(transparent)]
pub struct Thread {
    raw: UnsafeCell<lua_State>,
}

impl Thread {
    /// Construct a reference to [`Thread`] from a raw C pointer to a Lua state.
    ///
    /// # Safety
    /// `l` must point to a valid Lua state (`lua_State *` in C), for the
    /// duration specified by `'a`.
    pub unsafe fn from_ptr<'a>(l: *mut lua_State) -> &'a Self {
        unsafe { &*(l as *mut Self) }
    }

    /// Construct a _mutable_ reference to [`Thread`] from a raw C pointer to a
    /// Lua state.
    ///
    /// # Safety
    /// `l` must point to a valid Lua state (`lua_State *` in C), for the
    /// duration specified by `'a`.
    ///
    /// **You must also, however, abide by Rust's aliasing rules.**
    /// This means that you must guarantee that there
    /// may not be two `&mut Thread`s that point to the same state,
    /// nor a `&Thread` and a `&mut Thread`.
    pub unsafe fn from_ptr_mut<'a>(l: *mut lua_State) -> &'a mut Self {
        unsafe { &mut *(l as *mut Self) }
    }

    /// Return the raw C pointer that represents the underlying Lua state.
    pub fn as_ptr(&mut self) -> *mut lua_State {
        self.raw.get()
    }

    /// Return the raw C pointer that represents the underlying Lua state.
    ///
    /// # Safety
    /// The returned pointer must only be used for *inspecting* the state.
    pub unsafe fn as_ptr_inspect(&self) -> *mut lua_State {
        self.raw.get()
    }

    /// Return the raw C pointer that represents the underlying Lua state.
    ///
    /// # Safety
    /// The returned pointer must only be used for actions that wouldn't invalidate earlier references.
    /// The following situations are considered to be OK:
    /// - Pushing a value on the stack (the GC is not run).
    /// - Causing a error.
    pub unsafe fn as_ptr_no_gc(&self) -> *mut lua_State {
        self.raw.get()
    }

    /// Return a context that allows to run code
    /// that can restart the GC and potentially invalidate pointers.
    pub fn managed(&mut self) -> Managed<'_> {
        Managed {
            l: self.as_ptr(),
            _life: PhantomData,
        }
    }

    /// This is the same as [`Thread::managed`], however it doesn't borrow
    /// mutably by assuming that the garbage collector will not collect (and
    /// thus invalidate) any outside references.
    ///
    /// # Safety
    /// The returned context must not be used for any operations
    /// which may cause the garbage collector to invalidate pointers.
    ///
    /// For example, if performing arithmetic does not trigger any metamethods,
    /// or if returned pointers are still on the Lua stack,
    /// then this guarantee is not broken.
    pub unsafe fn managed_no_gc(&self) -> Managed<'_> {
        Managed {
            l: unsafe { self.as_ptr_no_gc() },
            _life: PhantomData,
        }
    }

    /// Close all active to-be-closed variables in the main thread, release all
    /// objects (calling the corresponding garbage-collection metamethods, if
    /// any), and free all dynamic memory used by this [`Thread`].
    ///
    /// # Safety
    /// This [`Thread`] must not be used for any further API calls, as the
    /// underlying Lua pointer becomes invalid after this call.
    pub unsafe fn close_as_main(&mut self) {
        unsafe { lua_close(self.as_ptr()) }
    }

    /// Reset a thread, cleaning its call stack and closing all pending
    /// to-be-closed variables.
    ///
    /// Returns a status code: [`Status::Ok`] for no errors in the thread
    /// (either the original error that stopped the thread or errors in closing
    /// methods), or an error status otherwise.
    ///
    /// In case of error, leaves the error object on the top of its own stack.
    pub fn close_as_coroutine(&mut self) -> Status {
        unsafe { Status::from_c_int_unchecked(lua_resetthread(self.as_ptr())) }
    }

    /// This behaves similarly to [`Thread::close_as_coroutine`], but allows to specify `from`,
    /// which represents the coroutine that is resetting this one.
    pub fn close_as_coroutine_from(&mut self, from: &Self) -> Status {
        unsafe { Status::from_c_int_unchecked(lua_closethread(self.as_ptr(), from.as_ptr_no_gc())) }
    }

    /// Set a new panic function and return the old one.
    pub fn at_c_panic(&self, func: Option<lua_CFunction>) -> Option<lua_CFunction> {
        unsafe { lua_atpanic(self.as_ptr_no_gc(), func) }
    }

    /// Raise a Lua error, using the value on the top of the stack as the error object.
    ///
    /// This function does a long jump, and therefore never returns.
    pub fn error(&self) -> ! {
        unsafe { lua_error(self.as_ptr_no_gc()) }
    }

    /// Restart the garbage collector.
    ///
    /// This by itself does not run a collection.
    pub fn restart_gc(&self) {
        unsafe { lua_gc(self.as_ptr_no_gc(), GcTask::Restart as _) };
    }

    /// Stop the garbage collector.
    pub fn stop_gc(&self) {
        unsafe { lua_gc(self.as_ptr_no_gc(), GcTask::Stop as _) };
    }

    /// Return the current amount of memory (in kilobytes) in use by this [`Thread`].
    pub fn mem_kbytes(&self) -> c_uint {
        unsafe { lua_gc(self.as_ptr_inspect(), GcTask::CountKbytes as _) }.clamp(0, c_int::MAX) as _
    }

    /// Return the remainder of dividing the current amount of bytes of memory in use by this [`Thread`] by `1024`.
    pub fn mem_byte_remainder(&self) -> c_uint {
        unsafe { lua_gc(self.as_ptr_inspect(), GcTask::CountBytesRem as _) }.clamp(0, c_int::MAX)
            as _
    }

    /// Return true if the collector is running (i.e. not stopped).
    pub fn is_gc_running(&self) -> bool {
        (unsafe { lua_gc(self.as_ptr_inspect(), GcTask::IsRunning as _) }) != 0
    }

    /// Change the collector to either incremental or generational mode (see also [`GcMode`]) with the given parameters.
    pub fn switch_gc_to(&mut self, gc: GcMode) {
        match gc {
            GcMode::Incremental {
                pause,
                step_multiplier,
                step_size,
            } => unsafe {
                lua_gc(
                    self.as_ptr(),
                    GcTask::ToIncremental as _,
                    pause as c_int,
                    step_multiplier as c_int,
                    step_size,
                )
            },
            GcMode::Generational {
                minor_mul,
                major_mul,
            } => unsafe {
                lua_gc(
                    self.as_ptr(),
                    GcTask::ToGenerational as _,
                    minor_mul as c_int,
                    major_mul as c_int,
                )
            },
        };
    }

    /// Convert the acceptable index `idx` into an equivalent absolute index
    /// (that is, one that does not depend on the stack size).
    pub fn abs_index(&self, idx: AcceptableIndex) -> Option<AbsIndex> {
        AbsIndex::new(unsafe { lua_absindex(self.as_ptr_inspect(), idx) })
    }

    /// Ensure that the stack has space for at least `n` extra elements.
    /// That is, that you can safely push up to `n` values into it.
    ///
    /// Returns `false` if it cannot fulfill the request, either because it
    /// would cause the stack to be greater than a fixed maximum size (typically
    /// at least several thousand elements) or because it cannot allocate memory
    /// for the extra space.
    ///
    /// This function never shrinks the stack; if the stack already has space
    /// for the extra elements, it is left unchanged.
    pub fn test_stack(&self, n: c_uint) -> bool {
        (unsafe { lua_checkstack(self.as_ptr_inspect(), n as _) }) != 0
    }

    /// Copy the element at `from_idx` into the valid index `to_idx`, replacing
    /// the value at that position.
    ///
    /// Values at other positions are not affected.
    pub fn copy(&self, from_idx: AcceptableIndex, to_idx: AcceptableIndex) {
        unsafe { lua_copy(self.as_ptr_inspect(), from_idx, to_idx) }
    }

    /// Return the memory-allocation function of this [`Thread`] along with the
    /// opaque pointer given when the memory-allocator function was set.
    pub fn get_alloc_fn(&self) -> (lua_Alloc, *mut c_void) {
        let mut ud = null_mut();
        let alloc_fn = unsafe { lua_getallocf(self.as_ptr_inspect(), &mut ud as *mut *mut c_void) };
        (alloc_fn, ud)
    }

    /// Push onto the stack the `n`-th user value associated with the full
    /// userdata at the given index and returns the type of the pushed value.
    ///
    /// If the userdata does not have that value, push `nil` and return [`Type::None`].
    pub fn get_i_uservalue(&self, ud_index: c_int, n: c_int) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_getiuservalue(self.as_ptr_no_gc(), ud_index, n)) }
    }

    /// If the value at the given index has a metatable, push that metatable
    /// onto the stack and return `true`. Otherwise, push nothing and return
    /// `false`.
    pub fn get_metatable(&self, obj_index: c_int) -> bool {
        (unsafe { lua_getmetatable(self.as_ptr_no_gc(), obj_index) }) != 0
    }

    /// Return the index of the top element in the stack.
    ///
    /// Because indices start at `1`, this result is equal to the number of
    /// elements in the stack; in particular, `0` means an empty stack.
    pub fn top(&self) -> c_int {
        unsafe { lua_gettop(self.as_ptr_inspect()) }
    }

    /// Move the top element into the given valid index, shifting up the
    /// elements above that index to open space.
    ///
    /// This function cannot be called with a pseudo-index, because a
    /// pseudo-index is not an actual stack position.
    pub fn insert(&self, index: c_int) {
        unsafe { lua_insert(self.as_ptr_no_gc(), index) }
    }

    lua_is! {
        /// Return `true` if the value at the given index is a boolean.
        pub fn is_boolean(&self, index: AcceptableIndex) -> bool for lua_isboolean -> bool;

        /// Return `true` if the value at the given index is a C function.
        pub fn is_c_function(&self, index: AcceptableIndex) -> bool
            for lua_iscfunction -> c_int;

        /// Return `true` if the value at the given index is a function (either
        /// C or Lua).
        pub fn is_function(&self, index: AcceptableIndex) -> bool
            for lua_isfunction -> bool;

        /// Return `true` if the value at the given index is an integer.
        pub fn is_integer(&self, index: AcceptableIndex) -> bool
            for lua_isinteger -> c_int;

        /// Return `true` if the value at the given index is a light userdata.
        pub fn is_light_userdata(&self, index: AcceptableIndex) -> bool
            for lua_islightuserdata -> bool;

        /// Return `true` if the value at the given index is `nil`.
        pub fn is_nil(&self, index: AcceptableIndex) -> bool for lua_isnil -> bool;

        /// Return `true` if the value at the given index is not valid.
        pub fn is_none(&self, index: AcceptableIndex) -> bool for lua_isnone -> bool;

        /// Return `true` if the value at the given index is not valid or is
        /// `nil`.
        pub fn is_none_or_nil(&self, index: AcceptableIndex) -> bool
            for lua_isnoneornil -> bool;
        /// Return `true` if the value at the given index is a number.

        pub fn is_number(&self, index: AcceptableIndex) -> bool for lua_isnumber -> c_int;

        /// Return `true` if the value at the given index is a string *or* a
        /// number, which is always convertible to a string.
        pub fn is_string(&self, index: AcceptableIndex) -> bool for lua_isstring -> c_int;

        /// Return `true` if the value at the given index is a table.
        pub fn is_table(&self, index: AcceptableIndex) -> bool for lua_istable -> bool;

        /// Return `true` if the value at the given index is a thread.
        pub fn is_thread(&self, index: AcceptableIndex) -> bool for lua_isthread -> bool;

        /// Return `true` if the value at the given index is a userdata (either
        /// full or light).
        pub fn is_userdata(&self, index: AcceptableIndex) -> bool
            for lua_isuserdata -> c_int;
    }

    /// Return `true` if the coroutine can yield.
    pub fn can_yield(&self) -> bool {
        (unsafe { lua_isyieldable(self.as_ptr_inspect()) }) != 0
    }

    /// Pop a key from the stack, and push a key–value pair from the table at
    /// the given index, the "next" pair after the given key.
    ///
    /// This function returns `true` while there are still elements to go
    /// through. If there are no more elements in the table, then this it
    /// returns `false` and pushes nothing.
    ///
    /// # Note on string conversion functions
    /// While traversing a table, avoid calling [`Managed::to_c_chars`] directly
    /// on a key, unless it is known that the key is actually a **string**.
    /// [`Managed::to_c_chars`] and other similar functions may change the value
    /// at the given index; this confuses the next call to [`Thread::next`].
    ///
    /// # Errors
    /// The underlying Lua state may raise an [error](crate::errors)
    /// if a given key is neither `nil` nor present in the table.
    pub fn next(&self, index: AcceptableIndex) -> bool {
        (unsafe { lua_next(self.as_ptr_no_gc(), index) }) != 0
    }

    /// Push a [`bool`] onto the stack.
    pub fn push_boolean(&self, value: bool) {
        unsafe { lua_pushboolean(self.as_ptr_no_gc(), if value { 1 } else { 0 }) }
    }

    /// Push a light C function onto the stack (that is, a C function with no upvalues).
    ///
    /// See also [`Managed::push_c_closure`].
    pub fn push_c_function(&self, func: lua_CFunction) {
        unsafe { lua_pushcfunction(self.as_ptr_no_gc(), func) }
    }

    /// Push the global environment onto the stack.
    pub fn push_global_table(&self) {
        unsafe { lua_pushglobaltable(self.as_ptr_no_gc()) }
    }

    /// Push an [`Integer`] onto the stack.
    pub fn push_integer(&self, value: Integer) {
        unsafe { lua_pushinteger(self.as_ptr_no_gc(), value) }
    }

    /// Push a light userdata onto the stack.
    ///
    /// A light userdata represents a plain pointer.
    /// It is a value, like a number:
    /// it is not created, it has no individual metatable,
    /// and it is not collected (as it was never created).
    ///
    /// A light userdata is equal to any light userdata with the same C address.
    ///
    /// # Safety
    /// `ptr` can be used arbitrarily in Lua,
    /// so this method should only be used for trusted code.
    pub unsafe fn push_light_userdata(&self, ptr: *mut c_void) {
        unsafe { lua_pushlightuserdata(self.as_ptr_no_gc(), ptr) }
    }

    /// Push `nil` onto the stack.
    pub fn push_nil(&self) {
        unsafe { lua_pushnil(self.as_ptr_no_gc()) }
    }

    /// Push a [`Number`] onto the stack.
    pub fn push_number(&self, value: Number) {
        unsafe { lua_pushnumber(self.as_ptr_no_gc(), value) }
    }

    /// Push the Lua thread represented by this [`Thread`] onto its own stack,
    /// and return `true` if this thread is the main thread
    /// (see also [`Lua`](crate::Lua)).
    pub fn push_thread(&self) -> bool {
        (unsafe { lua_pushthread(self.as_ptr_no_gc()) }) != 0
    }

    /// Push a copy of the element at the given index onto the stack.
    pub fn push_value(&self, index: AcceptableIndex) {
        unsafe { lua_pushvalue(self.as_ptr_no_gc(), index) }
    }

    /// Return `true` if the two values in indices `idx_a` and `idx_b` are
    /// primitively equal (that is, equal without calling the `__eq` metamethod).
    ///
    /// This also returns `false` if any of the indices are not valid.
    pub fn raw_equal(&self, idx_a: AcceptableIndex, idx_b: AcceptableIndex) -> bool {
        (unsafe { lua_rawequal(self.as_ptr_no_gc(), idx_a, idx_b) }) != 0
    }

    /// Without calling metamethods, push `t[k]`, where `t` is the value at the
    /// given index and `k` is the value on the top of the stack.
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_get(&self, tbl_index: AcceptableIndex) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_rawget(self.as_ptr_no_gc(), tbl_index)) }
    }

    /// Without calling metamethods, push `t[i]`, where `t` is the value at the
    /// given index.
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_get_i(&self, tbl_index: AcceptableIndex, i: Integer) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_rawgeti(self.as_ptr_no_gc(), tbl_index, i)) }
    }

    /// Without calling metamethods, push `t[ptr]`, where `t` is the value at
    /// the given index and `ptr` is the given pointer represented as a light
    /// userdata.
    ///
    /// # Safety
    /// The value at `tbl_index` must be a table.
    pub unsafe fn raw_get_p(&self, tbl_index: AcceptableIndex, ptr: *const c_void) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_rawgetp(self.as_ptr_no_gc(), tbl_index, ptr)) }
    }

    /// Return the raw "length" of the value at the given index.
    ///
    /// For strings, this is the string length;
    /// for tables, this is the result of the length operator (`#`) with no
    /// metamethods;
    /// for userdata, this is the size of the block of memory allocated for the
    /// userdata.
    /// For other values, this call returns `0`.
    pub fn raw_length(&self, index: AcceptableIndex) -> Unsigned {
        unsafe { lua_rawlen(self.as_ptr_no_gc(), index) }
    }

    /// Move the top element into the given valid index without shifting any
    /// element (therefore replacing the value at that given index),
    /// and then pop that top element.
    pub fn replace(&self, index: c_int) {
        unsafe { lua_replace(self.as_ptr_no_gc(), index) }
    }

    /// Rotate the stack elements between the valid index `index` and the top of
    /// the stack.
    ///
    /// The elements are rotated `n` positions in the direction of the top for a
    /// ositive `n`, or `-n` positions in the direction of the bottom for a
    /// negative `n`.
    /// The absolute value of `n` must not be greater than the size of the slice
    /// being rotated.
    ///
    /// This function cannot be called with a pseudo-index, because a
    /// pseudo-index is not an actual stack position.
    pub fn rotate(&self, index: c_int, n_values: c_int) {
        unsafe { lua_rotate(self.as_ptr_no_gc(), index, n_values) }
    }

    /// Pop a value from the stack and set it as the new `n`-th user value
    /// associated to the full userdata at the given index.
    ///
    /// Returns `false` if the userdata does not have that value.
    pub fn set_i_uservalue(&self, ud_index: c_int, n: c_int) -> bool {
        (unsafe { lua_setiuservalue(self.as_ptr_no_gc(), ud_index, n) }) != 0
    }

    /// Pop a table or `nil` from the stack and sets that value as the new
    /// metatable for the value at the given index. (`nil` means no metatable.)
    // NOTE: `lua_setmetatable` always returns a `1`, which isn't useful.
    pub fn set_metatable(&self, obj_index: c_int) {
        unsafe { lua_setmetatable(self.as_ptr_no_gc(), obj_index) };
    }

    /// Set the warning function to be used by Lua to emit warnings
    /// (see [`lua_WarnFunction`]).
    ///
    /// See also [`Thread::remove_warn_fn`].
    ///
    /// # Safety
    /// `warn_data` is the custom data to be passed to the warning function.
    /// It must be valid for `warn`.
    pub unsafe fn set_warn_fn(&self, warn: lua_WarnFunction, warn_data: *mut c_void) {
        unsafe { lua_setwarnf(self.as_ptr_no_gc(), Some(warn), warn_data) }
    }

    /// Remove the warning function to be used by Lua to emit warnings.
    ///
    /// See also [`Thread::set_warn_fn`].
    pub fn remove_warn_fn(&self) {
        unsafe { lua_setwarnf(self.as_ptr_no_gc(), None, null_mut()) }
    }

    /// Return the status of the Lua thread represented by this [`Thread`].
    ///
    /// The status can be [`Status::Ok`] for a normal thread, an error variant
    /// if the thread finished the execution of a [`Managed::resume`] with an
    /// error, or [`Status::Yielded`] if the thread is suspended.
    ///
    /// Functions can only be called in threads with status [`Status::Ok`].
    /// Threads with status [`Status::Ok`] or [`Status::Yielded`] can be resumed
    /// (to start a new coroutine or resume an existing one).
    pub fn status(&self) -> Status {
        unsafe { Status::from_c_int_unchecked(lua_status(self.as_ptr_inspect())) }
    }

    /// Convert the Lua value at the given index to a [`bool`].
    ///
    /// Like all tests in Lua, this returns `true` for any Lua value different
    /// from `false` and `nil`; otherwise it returns `false`.
    ///
    /// If you want to accept only actual boolean values, use
    /// [`Thread::is_boolean`] to test the value's type first.
    pub fn to_boolean(&self, idx: c_int) -> bool {
        (unsafe { lua_toboolean(self.as_ptr_no_gc(), idx) }) != 0
    }

    /// Convert a value at the given index to a C function.
    /// If it is not one, return `None`.
    pub fn to_c_function(&self, index: c_int) -> Option<lua_CFunction> {
        unsafe { lua_tocfunction(self.as_ptr_no_gc(), index) }
    }

    /// Mark the given index in the stack as a to-be-closed slot.
    ///
    /// Like a to-be-closed variable in Lua, the value at that slot in the stack
    /// will be closed when it goes out of scope.
    /// Here, in the context of a C function, to go out of scope means that the
    /// running function returns to Lua, or there is an error, or the slot is
    /// removed from the stack through [`Managed::set_top`] or [`Managed::pop`],
    /// or there is a call to [`Managed::close_slot`].
    ///
    /// A slot marked as to-be-closed should not be removed from the stack by
    /// any other function in the API except [`Managed::set_top`] or
    /// [`Managed::pop`], unless previously deactivated by [`Managed::close_slot`].
    ///
    /// # Errors
    /// The underlying Lua state may raise a memory [error](crate::errors).
    ///
    /// # Safety
    /// This function should not be called for an index that is equal to or
    /// below an active to-be-closed slot.
    ///
    /// Note that, both in case of errors and of a regular return, by the time
    /// the `__close` metamethod runs, the C stack was already unwound, so that
    /// any automatic C variable declared in the calling function
    /// (e.g., a buffer) will be out of scope.
    pub unsafe fn to_close(&self, index: c_int) {
        unsafe { lua_toclose(self.as_ptr_no_gc(), index) }
    }

    /// This behaves exactly the same as [`Thread::to_integer_opt`], however the
    /// return value is `0` if an integer isn't present.
    pub fn to_integer(&self, idx: c_int) -> Integer {
        unsafe { lua_tointeger(self.as_ptr_no_gc(), idx) }
    }

    /// Convert the Lua value at the given index to the signed integral type
    /// [`Integer`].
    ///
    /// The Lua value must be an integer, or a number or string convertible to
    /// an integer. Otherwise, this function returns `None`.
    pub fn to_integer_opt(&self, idx: c_int) -> Option<Integer> {
        let mut is_num = 0;
        let result = unsafe { lua_tointegerx(self.as_ptr_no_gc(), idx, &mut is_num as *mut _) };
        (is_num != 0).then_some(result)
    }

    /// This behaves exactly the same as [`Thread::to_number_opt`], however the
    /// return value is `0.0` if a number isn't present.
    pub fn to_number(&self, idx: c_int) -> Number {
        unsafe { lua_tonumber(self.as_ptr_no_gc(), idx) }
    }

    /// Convert the Lua value at the given index to the floating-point number
    /// type [`Number`].
    ///
    /// The Lua value must be a number or string convertible to a number.
    /// Otherwise, this function returns `None`.
    pub fn to_number_opt(&self, idx: c_int) -> Option<Number> {
        let mut is_num = 0;
        let result = unsafe { lua_tonumberx(self.as_ptr_no_gc(), idx, &mut is_num as *mut _) };
        (is_num != 0).then_some(result)
    }

    /// Convert the value at the given index to a generic C pointer
    /// ([`*const c_void`](c_void)).
    ///
    /// The value can be a userdata, a table, a thread, a string, or a function;
    /// otherwise, this function returns null.
    ///
    /// Different objects will give different pointers.
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    pub fn to_pointer(&self, idx: c_int) -> *const c_void {
        unsafe { lua_topointer(self.as_ptr_no_gc(), idx) }
    }

    /// Convert the value at the given index to a Lua thread, represented by a
    /// `*mut`[`lua_State`].
    ///
    /// The value must be a thread; otherwise, the function returns null.
    pub fn to_thread(&self, index: c_int) -> *mut lua_State {
        unsafe { lua_tothread(self.as_ptr_no_gc(), index) }
    }

    /// If the value at the given index is a light or full userdata, return the
    /// address it represents. Otherwise, return null.
    pub fn to_userdata(&self, idx: c_int) -> *mut c_void {
        unsafe { lua_touserdata(self.as_ptr_no_gc(), idx) }
    }

    /// Return the type of the value in the given valid index, or [`Type::None`]
    /// for a non-valid but acceptable index.
    pub fn type_of(&self, idx: c_int) -> Type {
        unsafe { Type::from_c_int_unchecked(lua_type(self.as_ptr_inspect(), idx)) }
    }

    /// Return the name of the type encoded by `type_tag`.
    pub fn type_name(&self, type_tag: Type) -> &CStr {
        unsafe { CStr::from_ptr(lua_typename(self.as_ptr_inspect(), type_tag as _)) }
    }

    /// Return the version number of the Lua core.
    pub fn version(&self) -> Number {
        unsafe { lua_version(self.as_ptr_inspect()) }
    }

    /// Emit a warning with the given message.
    ///
    /// A message in a call with `to_be_continued == true` should be continued
    /// in another call to this function.
    pub fn warning(&mut self, message: &CStr, to_be_continued: bool) {
        unsafe {
            lua_warning(
                self.as_ptr(),
                message.as_ptr(),
                if to_be_continued { 1 } else { 0 },
            )
        }
    }

    /// Exchange values between different threads of the same state.
    ///
    /// This function pops `n_values` values from the stack of this thread, and
    /// pushes them onto the stack of the thread `to`.
    pub fn xmove(&self, to: &Self, n_values: c_uint) {
        unsafe { lua_xmove(self.as_ptr_no_gc(), to.as_ptr_no_gc(), n_values as _) }
    }

    /// This behaves exactly like [`Thread::yield_k_with`], however there is no
    /// continuation.
    ///
    /// # Safety
    /// This function should be called *only* outside of hooks.
    /// It is Undefined Behavior if the code after a call to this function is
    /// reachable.
    pub unsafe fn yield_with(&self, n_results: c_int) -> ! {
        unsafe { lua_yield(self.as_ptr_no_gc(), n_results) }
    }

    /// This behaves exactly like [`Thread::yield_in_hook_k_with`], however
    /// there is no continuation.
    ///
    /// # Safety
    /// This function should be called *only* outside of hooks.
    /// It is Undefined Behavior if the code after a call to this function is
    /// unreachable.
    pub unsafe fn yield_in_hook_with(&self, n_results: c_int) {
        unsafe { lua_yield_in_hook(self.as_ptr_no_gc(), n_results) };
    }

    /// Yield this thread (like a coroutine).
    ///
    /// When this function is called, the running coroutine suspends its
    /// execution, and the call to [`Managed::resume`] that started this
    /// coroutine returns.
    ///
    /// The parameter `n_results` is the number of values from the stack that
    /// will be passed as results to [`Managed::resume`].
    ///
    /// When the coroutine is resumed again, Lua calls the given continuation
    /// function `continuation` to continue the execution of the C function that
    /// yielded.
    /// This continuation function receives the same stack from the previous
    /// function, with the `n_results` results removed and replaced by the
    /// arguments passed to [`Managed::resume`].
    /// Moreover, the continuation function receives the value `context` that
    /// was originally passed.
    ///
    /// Usually, this function does not return; when the coroutine eventually
    /// resumes, it continues executing the continuation function.
    /// However, there is one special case, which is when this function is
    /// called from inside a line or a count hook (see [`lua_Hook`]).
    /// In that case, [`Thread::yield_in_hook_with`] should be called
    /// (thus, no continuation) and no results, and the hook should return
    /// immediately after the call.
    /// Lua will yield and, when the coroutine resumes again, it will continue
    /// the normal execution of the (Lua) function that triggered the hook.
    ///
    /// # Errors
    /// The underlying Lua thread can raise an error
    /// if the function is called from a thread with a pending C call
    /// with no continuation function (what is called a C-call boundary),
    /// or it is called from a thread that is not running inside a resume
    /// (typically the main thread).
    ///
    /// # Safety
    /// This function should be called *only* outside of hooks.
    /// It is Undefined Behavior if the code after a call to this function is
    /// reachable.
    pub unsafe fn yield_k_with(
        &self,
        n_results: c_int,
        continuation: lua_KFunction,
        context: lua_KContext,
    ) -> ! {
        unsafe { lua_yieldk(self.as_ptr_no_gc(), n_results, context, Some(continuation)) }
    }

    /// This behaves exactly like [`Thread::yield_k_with`], however it should
    /// only be called in hooks.
    ///
    /// # Errors
    /// The underlying Lua thread can raise an error
    /// if the function is called from a thread with a pending C call
    /// with no continuation function (what is called a C-call boundary),
    /// or it is called from a thread that is not running inside a resume
    /// (typically the main thread).
    ///
    /// # Safety
    /// This function should be called *only* inside of hooks.
    pub unsafe fn yield_in_hook_k_with(
        &self,
        n_results: c_int,
        continuation: lua_KFunction,
        context: lua_KContext,
    ) {
        unsafe { lua_yieldk_in_hook(self.as_ptr_no_gc(), n_results, context, Some(continuation)) };
    }

    /// Return the current hook count.
    pub fn hook_count(&self) -> c_int {
        unsafe { lua_gethookcount(self.as_ptr_inspect()) }
    }

    /// Return the current hook [`EventMask`].
    pub fn hook_mask(&self) -> EventMask {
        unsafe { EventMask(lua_gethookmask(self.as_ptr_inspect())) }
    }

    /// Get information about the `n`-th upvalue of the closure at index
    /// `func_index`.
    ///
    /// This function pushes the upvalue's value onto the stack and returns its
    /// name. Returns `None` (and pushes nothing) when the index `n` is greater
    /// than the number of upvalues.
    pub fn get_upvalue(&self, func_index: c_int, n: u8) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(lua_getupvalue(self.as_ptr_inspect(), func_index, n as _)) }
    }

    /// Set the value of a closure's upvalue and return its name.
    ///
    /// Returns `None` (and pops nothing) when the index `n` is greater than the
    /// number of upvalues.
    ///
    /// This function assigns the value on the top of the stack to the upvalue.
    /// It also pops the value from the stack.
    pub fn set_upvalue(&self, func_index: c_int, n: u8) -> Option<&CStr> {
        unsafe { crate::util::opt_c_str(lua_setupvalue(self.as_ptr_inspect(), func_index, n as _)) }
    }

    /// Return a unique identifier for the upvalue numbered `n` from the closure
    /// at index `func_index`.
    ///
    /// These unique identifiers allow a program to check whether different
    /// closures share upvalues.
    /// Lua closures that share an upvalue (that is, that access a same external
    /// local variable) will return identical ids for those upvalue indices.
    ///
    /// # Safety
    /// The returned pointer may only be used for comparisons.
    pub unsafe fn upvalue_id(&self, func_index: c_int, n: u8) -> *mut c_void {
        unsafe { lua_upvalueid(self.as_ptr_inspect(), func_index, n as _) }
    }

    /// Make the
    /// `n_into`-th upvalue of the Lua closure at index `func_into_index`
    /// refer to the
    /// `n_from`-th upvalue of the Lua closure at index `func_from_index`.
    pub fn upvalue_join(&self, func_into_index: i32, n_into: u8, func_from_index: i32, n_from: u8) {
        unsafe {
            lua_upvaluejoin(
                self.as_ptr_no_gc(),
                func_into_index,
                n_into as _,
                func_from_index,
                n_from as _,
            )
        }
    }
}
