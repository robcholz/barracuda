use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    mem::MaybeUninit,
};

use barracuda_lua::Lua;
use embedded_alloc::LlffHeap;

const MINIMUM_HEAP_BYTES: usize = 1_024;

struct SlotMemory {
    allocator: LlffHeap,
    _backing: Box<[MaybeUninit<u8>]>,
}

struct MemorySlot {
    memory: RefCell<Option<SlotMemory>>,
    bytes: usize,
    claimed: Cell<bool>,
}

impl MemorySlot {
    const fn new(bytes: usize) -> Self {
        Self {
            memory: RefCell::new(None),
            bytes,
            claimed: Cell::new(false),
        }
    }

    fn prepare(&self) -> Result<(), VmMemoryPoolError> {
        if self.memory.borrow().is_some() {
            return Ok(());
        }
        let mut backing = Vec::new();
        backing
            .try_reserve_exact(self.bytes)
            .map_err(|_error| VmMemoryPoolError::Allocation)?;
        backing.resize(self.bytes, MaybeUninit::uninit());
        let mut backing = backing.into_boxed_slice();
        let allocator = LlffHeap::empty();
        unsafe { allocator.init(backing.as_mut_ptr().cast::<u8>() as usize, backing.len()) };
        *self.memory.borrow_mut() = Some(SlotMemory {
            allocator,
            _backing: backing,
        });
        Ok(())
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
            slots.push(MemorySlot::new(bytes));
        }
        Ok(Self {
            inner: Rc::new(MemoryPoolInner {
                slots: slots.into_boxed_slice(),
            }),
        })
    }

    pub(crate) fn acquire(&self) -> Result<Option<VmMemoryLease>, VmMemoryPoolError> {
        let Some((index, slot)) = self
            .inner
            .slots
            .iter()
            .enumerate()
            .find(|(_index, slot)| !slot.claimed.replace(true))
        else {
            return Ok(None);
        };
        if let Err(error) = slot.prepare() {
            slot.claimed.set(false);
            return Err(error);
        }
        Ok(Some(VmMemoryLease {
            pool: self.clone(),
            index,
        }))
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
        let memory = self.pool.inner.slots[self.index].memory.borrow();
        let allocator = memory
            .as_ref()
            .map(|memory| &raw const memory.allocator)
            .ok_or_else(|| barracuda_lua::Error::runtime("VM memory slot was not prepared"))?;
        unsafe { Lua::new_with_allocator(allocator) }
    }
}

impl Drop for VmMemoryLease {
    fn drop(&mut self) {
        self.pool.inner.slots[self.index].claimed.set(false);
    }
}

/// Failure while creating the VM's reusable Lua memory pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmMemoryPoolError {
    /// At least one allocator slot is required.
    #[error("VM memory pool must contain at least one slot")]
    Empty,
    /// The requested concurrency exceeds statically allocated Embassy slots.
    #[error("VM memory slot count exceeds the static task capacity")]
    TooManySlots,
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
        let lease = pool
            .acquire()
            .expect("prepare allocator slot")
            .expect("acquire allocator slot");
        assert!(pool.acquire().expect("inspect allocator pool").is_none());
        let mut lua = unsafe { lease.create_lua() }.expect("create pooled Lua state");
        assert_eq!(lua.load("return 42").eval::<i64>().expect("run Lua"), 42);
        drop(lua);
        drop(lease);

        assert!(pool.acquire().expect("reuse allocator slot").is_some());
    }
}
