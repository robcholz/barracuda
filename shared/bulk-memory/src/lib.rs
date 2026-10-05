//! POD-only containers backed by a Platform-selected bulk-memory allocator.
//!
//! Platforms install the allocator that represents their high-capacity memory
//! domain. Callers use [`BulkVec`] and [`BulkBox`] without receiving or naming
//! that allocator, so non-POD control state cannot be placed in the domain by
//! safe code.

#![no_std]

use core::{alloc::Layout, fmt, ops::Deref, ops::DerefMut, ptr::NonNull};

use allocator_api2::{
    alloc::{AllocError, Allocator, Global},
    boxed::Box as AllocBox,
    collections::TryReserveError,
    vec::Vec as AllocVec,
};
use bytemuck::Pod;
use portable_atomic::{AtomicPtr, Ordering};

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

/// The active bulk-memory domain could not satisfy an allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkAllocError;

impl fmt::Display for BulkAllocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bulk memory allocation failed")
    }
}

impl core::error::Error for BulkAllocError {}

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
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: AllocVec::with_capacity_in(capacity, BulkAllocator::active()),
        }
    }

    /// Creates an empty sequence with storage for exactly `capacity` elements.
    ///
    /// # Errors
    ///
    /// Returns [`BulkAllocError`] when the bulk-memory domain is exhausted.
    pub fn try_with_capacity(capacity: usize) -> Result<Self, BulkAllocError> {
        let mut values = Self::new();
        values
            .try_reserve_exact(capacity)
            .map_err(|_error| BulkAllocError)?;
        Ok(values)
    }

    /// Copies `values` into a new exactly sized sequence.
    ///
    /// # Errors
    ///
    /// Returns [`BulkAllocError`] when the bulk-memory domain is exhausted.
    pub fn try_from_slice(values: &[T]) -> Result<Self, BulkAllocError> {
        let mut copy = Self::try_with_capacity(values.len())?;
        copy.extend_from_slice(values);
        Ok(copy)
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

    /// Returns the initialized elements as a shared slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.inner.as_slice()
    }

    /// Returns the initialized elements as a mutable slice.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.inner.as_mut_slice()
    }

    /// Removes all elements while retaining allocated storage.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Reserves capacity for at least `additional` more elements.
    pub fn reserve(&mut self, additional: usize) {
        self.inner.reserve(additional);
    }

    /// Reserves the minimum capacity for `additional` more elements.
    pub fn reserve_exact(&mut self, additional: usize) {
        self.inner.reserve_exact(additional);
    }

    /// Attempts to reserve capacity for at least `additional` more elements.
    ///
    /// # Errors
    ///
    /// Returns an error when the new capacity overflows or cannot be allocated.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.inner.try_reserve(additional)
    }

    /// Attempts to reserve the minimum capacity for `additional` more elements.
    ///
    /// # Errors
    ///
    /// Returns an error when the new capacity overflows or cannot be allocated.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.inner.try_reserve_exact(additional)
    }

    /// Appends one element, growing the allocation when necessary.
    pub fn push(&mut self, value: T) {
        self.inner.push(value);
    }

    /// Appends a copied slice, growing the allocation when necessary.
    pub fn extend_from_slice(&mut self, values: &[T]) {
        self.inner.extend_from_slice(values);
    }

    /// Resizes the sequence, copying `value` into newly initialized elements.
    pub fn resize(&mut self, new_len: usize, value: T) {
        self.inner.resize(new_len, value);
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
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            inner: AllocBox::new_in(value, BulkAllocator::active()),
        }
    }
}

impl<T: Pod> BulkBox<[T]> {
    /// Allocates a zero-filled POD slice.
    #[must_use]
    pub fn new_zeroed_slice(length: usize) -> Self {
        let value = AllocBox::<[T], _>::new_zeroed_slice_in(length, BulkAllocator::active());
        // SAFETY: `Pod` extends `Zeroable`, so an all-zero value is valid for
        // every element in the allocation.
        let value = unsafe { value.assume_init() };
        Self { inner: value }
    }

    /// Attempts to allocate a zero-filled POD slice.
    ///
    /// # Errors
    ///
    /// Returns [`BulkAllocError`] when the bulk-memory domain is exhausted.
    pub fn try_new_zeroed_slice(length: usize) -> Result<Self, BulkAllocError> {
        let mut values = BulkVec::try_with_capacity(length)?;
        values.resize(length, bytemuck::Zeroable::zeroed());
        Ok(values.into_boxed_slice())
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
            portable_atomic::Ordering::Release,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{BulkBox, BulkVec};

    #[test]
    fn fallible_constructors_allocate_exact_storage() {
        let values = BulkVec::try_from_slice(&[1_u8, 2, 3]).expect("allocate copy");
        assert_eq!(values.as_slice(), &[1, 2, 3]);
        assert_eq!(values.capacity(), 3);

        let zeroed = BulkBox::<[u16]>::try_new_zeroed_slice(4).expect("allocate zeroed slice");
        assert_eq!(&*zeroed, &[0, 0, 0, 0]);
    }

    #[test]
    fn impossible_allocations_report_errors() {
        assert!(BulkVec::<u64>::try_with_capacity(usize::MAX).is_err());
        assert!(BulkBox::<[u64]>::try_new_zeroed_slice(usize::MAX).is_err());
    }
}
