//! POD-only containers backed by a Platform-selected bulk-memory allocator.
//!
//! Platforms install the allocator that represents their high-capacity memory
//! domain. Callers use [`BulkVec`] and [`BulkBox`] without receiving or naming
//! that allocator, so non-POD control state cannot be placed in the domain by
//! safe code.

#![no_std]

use core::{
    alloc::Layout,
    fmt,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};

use allocator_api2::{
    alloc::{AllocError, Allocator, Global},
    boxed::Box as AllocBox,
    collections::TryReserveError,
    vec::Vec as AllocVec,
};
use bytemuck::Pod;

static GLOBAL_ALLOCATOR: Global = Global;
static GLOBAL_BACKEND: platform::Backend = platform::Backend::new(&GLOBAL_ALLOCATOR);
static ACTIVE_BACKEND: AtomicPtr<platform::Backend> = AtomicPtr::new(core::ptr::null_mut());

fn active_backend() -> &'static platform::Backend {
    let backend = ACTIVE_BACKEND.load(Ordering::Acquire);
    if backend.is_null() {
        &GLOBAL_BACKEND
    } else {
        // SAFETY: `platform::install` accepts only a static backend, and an
        // installed backend is never mutated or freed.
        unsafe { &*backend }
    }
}

#[derive(Clone, Copy)]
struct BulkAllocator {
    backend: &'static platform::Backend,
}

impl BulkAllocator {
    fn active() -> Self {
        Self {
            backend: active_backend(),
        }
    }
}

unsafe impl Allocator for BulkAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.backend.allocator.allocate(layout)
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        // SAFETY: allocator-api2 passes the pointer and layout back to the
        // allocator instance that produced the allocation.
        unsafe { self.backend.allocator.deallocate(pointer, layout) }
    }
}

mod private {
    pub trait Sealed {}

    impl<T: bytemuck::Pod> Sealed for T {}
    impl<T: bytemuck::Pod> Sealed for [T] {}
}

/// A value or slice whose complete representation is safe in bulk memory.
///
/// This sealed trait is implemented for [`Pod`] values and slices of `Pod`
/// values. It excludes atomics, locks, futures, pointers, and other control
/// structures from the bulk-memory domain.
pub trait BulkData: private::Sealed {}

impl<T: Pod> BulkData for T {}
impl<T: Pod> BulkData for [T] {}

/// A fallible bulk-memory construction failure.
#[derive(Debug)]
pub enum BulkMemoryError {
    /// The allocator rejected a direct allocation.
    Allocation(AllocError),
    /// A vector capacity request overflowed or could not be allocated.
    Capacity(TryReserveError),
}

impl fmt::Display for BulkMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation(error) => error.fmt(formatter),
            Self::Capacity(error) => error.fmt(formatter),
        }
    }
}

impl core::error::Error for BulkMemoryError {}

impl From<AllocError> for BulkMemoryError {
    fn from(error: AllocError) -> Self {
        Self::Allocation(error)
    }
}

impl From<TryReserveError> for BulkMemoryError {
    fn from(error: TryReserveError) -> Self {
        Self::Capacity(error)
    }
}

/// A growable POD sequence allocated from the active Platform bulk-memory
/// domain.
pub struct BulkVec<T: Pod> {
    inner: AllocVec<T, BulkAllocator>,
}

impl<T: Pod> BulkVec<T> {
    /// Creates an empty sequence without allocating.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: AllocVec::new_in(BulkAllocator::active()),
        }
    }

    /// Creates an empty sequence with at least `capacity` elements of storage.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested capacity overflows or the active
    /// bulk-memory allocator cannot satisfy it.
    pub fn try_with_capacity(capacity: usize) -> Result<Self, BulkMemoryError> {
        let mut value = Self::new();
        value.inner.try_reserve_exact(capacity)?;
        Ok(value)
    }

    /// Returns the number of initialized elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns whether the sequence contains no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the number of elements available before reallocation.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Removes all elements while retaining allocated storage.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Reserves capacity for at least `additional` more elements.
    ///
    /// # Errors
    ///
    /// Returns an error when the new capacity overflows or cannot be allocated.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), BulkMemoryError> {
        self.inner.try_reserve(additional)?;
        Ok(())
    }

    /// Reserves the minimum capacity for `additional` more elements.
    ///
    /// # Errors
    ///
    /// Returns an error when the new capacity overflows or cannot be allocated.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), BulkMemoryError> {
        self.inner.try_reserve_exact(additional)?;
        Ok(())
    }

    /// Appends one element, growing the allocation when necessary.
    ///
    /// # Errors
    ///
    /// Returns an error without appending when growth cannot be allocated.
    pub fn try_push(&mut self, value: T) -> Result<(), BulkMemoryError> {
        if self.inner.len() == self.inner.capacity() {
            self.inner.try_reserve(1)?;
        }
        self.inner.push(value);
        Ok(())
    }

    /// Appends a copied slice, growing the allocation when necessary.
    ///
    /// # Errors
    ///
    /// Returns an error without appending when growth cannot be allocated.
    pub fn try_extend_from_slice(&mut self, values: &[T]) -> Result<(), BulkMemoryError> {
        self.inner.try_reserve(values.len())?;
        self.inner.extend_from_slice(values);
        Ok(())
    }

    /// Removes and returns the final element, if present.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop()
    }

    /// Converts the vector into a bulk-memory boxed slice.
    #[must_use]
    pub fn into_boxed_slice(self) -> BulkBox<[T]> {
        BulkBox {
            inner: self.inner.into_boxed_slice(),
        }
    }
}

impl<T: Pod> Default for BulkVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Pod> Deref for BulkVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.inner.as_slice()
    }
}

impl<T: Pod> DerefMut for BulkVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut_slice()
    }
}

impl<T: Pod + fmt::Debug> fmt::Debug for BulkVec<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(formatter)
    }
}

/// A POD value or POD slice allocated from the active Platform bulk-memory
/// domain.
pub struct BulkBox<T: BulkData + ?Sized> {
    inner: AllocBox<T, BulkAllocator>,
}

impl<T: Pod> BulkBox<T> {
    /// Allocates and stores one POD value.
    ///
    /// # Errors
    ///
    /// Returns an error when the active bulk-memory allocator cannot satisfy
    /// the value's layout.
    pub fn try_new(value: T) -> Result<Self, BulkMemoryError> {
        Ok(Self {
            inner: AllocBox::try_new_in(value, BulkAllocator::active())?,
        })
    }
}

impl<T: Pod> BulkBox<[T]> {
    /// Allocates a zero-filled POD slice.
    ///
    /// # Errors
    ///
    /// Returns an error when the length overflows or the active bulk-memory
    /// allocator cannot satisfy the slice layout.
    pub fn try_zeroed_slice(length: usize) -> Result<Self, BulkMemoryError> {
        let value = AllocBox::<[T], _>::try_new_zeroed_slice_in(length, BulkAllocator::active())?;
        // SAFETY: `Pod` extends `Zeroable`, so an all-zero value is valid for
        // every element in the allocation.
        let value = unsafe { value.assume_init() };
        Ok(Self { inner: value })
    }
}

impl<T: BulkData + ?Sized> Deref for BulkBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: BulkData + ?Sized> DerefMut for BulkBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T: BulkData + fmt::Debug + ?Sized> fmt::Debug for BulkBox<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(formatter)
    }
}

/// Platform-only backend installation surface.
///
/// Business code should use [`BulkVec`] and [`BulkBox`] rather than importing
/// this module.
pub mod platform {
    use allocator_api2::alloc::Allocator;

    /// One process-lifetime allocator selected by a Platform implementation.
    pub struct Backend {
        pub(super) allocator: &'static (dyn Allocator + Sync),
    }

    impl Backend {
        /// Wraps a process-lifetime Platform allocator.
        #[must_use]
        pub const fn new(allocator: &'static (dyn Allocator + Sync)) -> Self {
            Self { allocator }
        }
    }

    /// Selects the backend used by subsequently constructed bulk containers.
    ///
    /// Existing containers retain the backend that allocated them, so a
    /// Platform may install its backend during bootstrap without invalidating
    /// earlier allocations made by a host harness.
    pub fn install(backend: &'static Backend) {
        super::ACTIVE_BACKEND.store(
            core::ptr::from_ref(backend).cast_mut(),
            core::sync::atomic::Ordering::Release,
        );
    }

    /// Selects the process global allocator as the Platform bulk-memory domain.
    ///
    /// Host Platforms and devices without a distinct external-memory allocator
    /// use this backend while preserving the same caller-facing container API.
    pub fn install_global() {
        install(&super::GLOBAL_BACKEND);
    }
}
