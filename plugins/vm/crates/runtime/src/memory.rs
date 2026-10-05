use alloc::{boxed::Box, rc::Rc};
use core::cell::{Cell, OnceCell};

use barracuda_bulk_memory::BulkBox;
use barracuda_lua::Lua;
use embedded_alloc::TlsfHeap;

const MINIMUM_HEAP_BYTES: usize = 1_024;

/// One Lua heap: a TLSF allocator over a bulk-memory backing buffer.
///
/// Lua stores only plain data in its heap; Rust userdata, callbacks, and
/// futures are boxed in the ordinary allocator. The backing buffer can
/// therefore live in the Platform bulk-memory domain (PSRAM on ESP32-S3).
struct Arena {
    allocator: TlsfHeap,
    _backing: BulkBox<[u8]>,
}

impl Arena {
    fn new(bytes: usize) -> Option<Box<Self>> {
        let mut backing = BulkBox::<[u8]>::try_new_zeroed_slice(bytes).ok()?;
        let allocator = TlsfHeap::empty();
        // SAFETY: the backing buffer is exclusively owned by this arena and
        // outlives the allocator, which is initialized exactly once.
        unsafe { allocator.init(backing.as_mut_ptr() as usize, backing.len()) };
        Some(Box::new(Self {
            allocator,
            _backing: backing,
        }))
    }
}

struct MemoryPoolInner {
    claimed: Box<[Cell<bool>]>,
    heap_bytes: usize,
}

/// Fixed number of execution slots whose Lua heaps are allocated per run.
#[derive(Clone)]
pub(crate) struct VmMemoryPool {
    inner: Rc<MemoryPoolInner>,
}

impl VmMemoryPool {
    pub(crate) fn new(slot_count: usize, heap_bytes: usize) -> Result<Self, VmMemoryPoolError> {
        if slot_count == 0 {
            return Err(VmMemoryPoolError::Empty);
        }
        if heap_bytes < MINIMUM_HEAP_BYTES {
            return Err(VmMemoryPoolError::TooSmall);
        }
        Ok(Self {
            inner: Rc::new(MemoryPoolInner {
                claimed: (0..slot_count).map(|_index| Cell::new(false)).collect(),
                heap_bytes,
            }),
        })
    }

    /// Claims one execution slot; its heap is allocated by [`VmMemoryLease::create_lua`].
    pub(crate) fn acquire(&self) -> Option<VmMemoryLease> {
        let index = self
            .inner
            .claimed
            .iter()
            .position(|claimed| !claimed.replace(true))?;
        Some(VmMemoryLease {
            pool: self.clone(),
            index,
            arena: OnceCell::new(),
        })
    }
}

pub(crate) struct VmMemoryLease {
    pool: VmMemoryPool,
    index: usize,
    arena: OnceCell<Box<Arena>>,
}

impl VmMemoryLease {
    /// Allocates this lease's Lua heap and creates a Lua state inside it.
    ///
    /// # Errors
    ///
    /// Returns a memory error when the bulk-memory domain cannot supply the heap.
    ///
    /// # Safety
    ///
    /// The lease must outlive the returned Lua state and all executions that own it.
    pub(crate) unsafe fn create_lua(&self) -> barracuda_lua::Result<Lua> {
        let arena = match self.arena.get() {
            Some(arena) => arena,
            None => {
                let arena = Arena::new(self.pool.inner.heap_bytes).ok_or_else(|| {
                    barracuda_lua::Error::memory("VM Lua heap could not be allocated")
                })?;
                self.arena.get_or_init(|| arena)
            }
        };
        let allocator = &raw const arena.allocator;
        unsafe { Lua::new_with_allocator(allocator) }
    }
}

impl Drop for VmMemoryLease {
    fn drop(&mut self) {
        self.pool.inner.claimed[self.index].set(false);
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

    /// Creates one Lua state with the production per-run heap size.
    pub fn with_default_heap() -> Result<Self, FixedMemoryLuaError> {
        Self::new(crate::runtime::VM_MEMORY_BYTES_PER_RUN)
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

/// Invalid VM memory pool configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmMemoryPoolError {
    /// At least one execution slot is required.
    #[error("VM memory pool must contain at least one slot")]
    Empty,
    /// The requested per-run heap is too small for the allocator.
    #[error("per-run Lua heap is too small")]
    TooSmall,
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

    #[test]
    fn unavailable_heap_is_a_lua_memory_error() {
        let pool = VmMemoryPool::new(1, usize::MAX).expect("create memory pool");
        let lease = pool.acquire().expect("acquire allocator slot");
        let result = unsafe { lease.create_lua() };
        assert!(matches!(
            result.map(drop).map_err(|error| error.kind()),
            Err(barracuda_lua::ErrorKind::Memory)
        ));
    }
}
