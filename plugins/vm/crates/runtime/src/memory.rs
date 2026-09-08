use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{cell::Cell, mem::MaybeUninit};

use barracuda_lua::Lua;
use embedded_alloc::TlsfHeap;

const MINIMUM_HEAP_BYTES: usize = 1_024;

struct MemorySlot {
    allocator: TlsfHeap,
    _backing: Box<[MaybeUninit<u8>]>,
    claimed: Cell<bool>,
}

impl MemorySlot {
    fn new(bytes: usize) -> Result<Self, VmMemoryPoolError> {
        let mut backing = Vec::new();
        backing
            .try_reserve_exact(bytes)
            .map_err(|_error| VmMemoryPoolError::Allocation)?;
        backing.resize(bytes, MaybeUninit::uninit());
        let mut backing = backing.into_boxed_slice();
        let allocator = TlsfHeap::empty();
        unsafe { allocator.init(backing.as_mut_ptr().cast::<u8>() as usize, backing.len()) };
        Ok(Self {
            allocator,
            _backing: backing,
            claimed: Cell::new(false),
        })
    }
}

struct MemoryPoolInner {
    slots: Box<[MemorySlot]>,
}

#[derive(Clone)]
pub(crate) struct VmMemoryPool {
    inner: Rc<MemoryPoolInner>,
}

impl VmMemoryPool {
    pub(crate) fn new(slot_count: usize, bytes: usize) -> Result<Self, VmMemoryPoolError> {
        if slot_count == 0 {
            return Err(VmMemoryPoolError::Empty);
        }
        if bytes < MINIMUM_HEAP_BYTES {
            return Err(VmMemoryPoolError::TooSmall);
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_count)
            .map_err(|_error| VmMemoryPoolError::Allocation)?;
        for _index in 0..slot_count {
            slots.push(MemorySlot::new(bytes)?);
        }
        Ok(Self {
            inner: Rc::new(MemoryPoolInner {
                slots: slots.into_boxed_slice(),
            }),
        })
    }

    pub(crate) fn acquire(&self) -> Option<VmMemoryLease> {
        let (index, _slot) = self
            .inner
            .slots
            .iter()
            .enumerate()
            .find(|(_index, slot)| !slot.claimed.replace(true))?;
        Some(VmMemoryLease {
            pool: self.clone(),
            index,
        })
    }
}

pub(crate) struct VmMemoryLease {
    pool: VmMemoryPool,
    index: usize,
}

impl VmMemoryLease {
    /// # Safety
    ///
    /// The lease must outlive the returned Lua state and all executions that own it.
    pub(crate) unsafe fn create_lua(&self) -> barracuda_lua::Result<Lua> {
        let allocator = &raw const self.pool.inner.slots[self.index].allocator;
        unsafe { Lua::new_with_allocator(allocator) }
    }
}

impl Drop for VmMemoryLease {
    fn drop(&mut self) {
        self.pool.inner.slots[self.index].claimed.set(false);
    }
}

/// Lua state backed by one owned VM memory slot for focused package tests.
#[cfg(feature = "test-fixture")]
pub struct FixedMemoryLua {
    lua: Lua,
    _lease: VmMemoryLease,
}

#[cfg(feature = "test-fixture")]
impl FixedMemoryLua {
    /// Creates one Lua state with the same allocator path as a normal VM run.
    pub fn new(bytes: usize) -> Result<Self, FixedMemoryLuaError> {
        let pool = VmMemoryPool::new(1, bytes)?;
        let lease = pool.acquire().ok_or(FixedMemoryLuaError::Unavailable)?;
        let lua = unsafe { lease.create_lua() }?;
        Ok(Self { lua, _lease: lease })
    }

    /// Mutably borrows the fixed-memory Lua state.
    pub fn lua_mut(&mut self) -> &mut Lua {
        &mut self.lua
    }
}

/// Failure constructing a fixed-memory Lua test state.
#[cfg(feature = "test-fixture")]
#[derive(Debug, thiserror::Error)]
pub enum FixedMemoryLuaError {
    /// The backing memory pool could not be created.
    #[error(transparent)]
    Pool(#[from] VmMemoryPoolError),
    /// The sole newly created slot could not be acquired.
    #[error("fixed VM memory slot is unavailable")]
    Unavailable,
    /// Lua could not initialize inside the fixed allocator.
    #[error(transparent)]
    Lua(#[from] barracuda_lua::Error),
}

/// Failure while creating the VM's reusable Lua memory pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmMemoryPoolError {
    /// At least one allocator slot is required.
    #[error("VM memory pool must contain at least one slot")]
    Empty,
    /// The requested per-VM heap is too small for the allocator.
    #[error("per-VM Lua heap is too small")]
    TooSmall,
    /// Backing memory for the allocator pool could not be reserved.
    #[error("failed to reserve VM memory pool backing storage")]
    Allocation,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{VmMemoryPool, VmMemoryPoolError};

    #[test]
    fn rejects_invalid_pool_capacity() {
        assert!(matches!(
            VmMemoryPool::new(0, 64 * 1024),
            Err(VmMemoryPoolError::Empty)
        ));
        assert!(matches!(
            VmMemoryPool::new(1, 1),
            Err(VmMemoryPoolError::TooSmall)
        ));
    }

    #[test]
    fn allocator_slot_is_exclusive_and_reusable() {
        let pool = VmMemoryPool::new(1, 64 * 1024).expect("create memory pool");
        let lease = pool.acquire().expect("acquire allocator slot");
        assert!(pool.acquire().is_none());
        let mut lua = unsafe { lease.create_lua() }.expect("create pooled Lua state");
        assert_eq!(lua.load("return 42").eval::<i64>().expect("run Lua"), 42);
        drop(lua);
        drop(lease);

        assert!(pool.acquire().is_some());
    }
}
