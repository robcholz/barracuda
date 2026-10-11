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
#[cfg(any(test, feature = "test-fixture"))]
pub struct FixedMemoryLua {
    lua: Lua,
    _lease: VmMemoryLease,
}

#[cfg(any(test, feature = "test-fixture"))]
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

/// Whether a Lua package fits the VM's per-package heap budgets.
///
/// Installed beside the built-in packages, `package` must keep at most
/// [`LUA_PACKAGE_HEAP_BUDGET_BYTES`](barracuda_vm_package_api::LUA_PACKAGE_HEAP_BUDGET_BYTES)
/// once garbage is collected, and its installation must succeed in the
/// smallest heap that holds the built-ins enlarged by that budget and
/// [`LUA_PACKAGE_INSTALL_PEAK_BYTES`](barracuda_vm_package_api::LUA_PACKAGE_INSTALL_PEAK_BYTES).
/// A package Plugin's tests assert it for every package it registers.
#[cfg(any(test, feature = "test-fixture"))]
#[must_use]
pub fn fits_package_budget(package: &impl barracuda_lua::Package) -> bool {
    use barracuda_vm_package_api::{LUA_PACKAGE_HEAP_BUDGET_BYTES, LUA_PACKAGE_INSTALL_PEAK_BYTES};

    let kept = retained_bytes(package);
    let peak = builtins_floor()
        .checked_add(LUA_PACKAGE_HEAP_BUDGET_BYTES)
        .and_then(|bytes| bytes.checked_add(LUA_PACKAGE_INSTALL_PEAK_BYTES));
    kept.is_some_and(|kept| kept <= LUA_PACKAGE_HEAP_BUDGET_BYTES)
        && peak.is_some_and(|bytes| installs(bytes, Some(package)))
}

/// Bytes `package` keeps in a run's heap beside the built-ins, after a full
/// garbage collection.
#[cfg(any(test, feature = "test-fixture"))]
fn retained_bytes(package: &impl barracuda_lua::Package) -> Option<usize> {
    let mut fixed = FixedMemoryLua::with_default_heap().ok()?;
    let _io = barracuda_vm_builtin_packages::BuiltinPackages::new(
        barracuda_vm_builtin_packages::math::SeedSource::unavailable(),
    )
    .install(fixed.lua_mut())
    .ok()?;
    fixed.lua_mut().collect_garbage();
    let before = fixed.lua_mut().memory_in_use();
    package.install(fixed.lua_mut()).ok()?;
    fixed.lua_mut().collect_garbage();
    Some(fixed.lua_mut().memory_in_use().saturating_sub(before))
}

/// Whether the built-in packages, and `package` when given, install into a
/// fresh heap of `bytes`.
#[cfg(any(test, feature = "test-fixture"))]
fn installs(bytes: usize, package: Option<&impl barracuda_lua::Package>) -> bool {
    let Ok(mut fixed) = FixedMemoryLua::new(bytes) else {
        return false;
    };
    let Ok(_io) = barracuda_vm_builtin_packages::BuiltinPackages::new(
        barracuda_vm_builtin_packages::math::SeedSource::unavailable(),
    )
    .install(fixed.lua_mut()) else {
        return false;
    };
    package.is_none_or(|package| package.install(fixed.lua_mut()).is_ok())
}

/// The smallest heap, to 256 bytes, that holds the built-in packages.
#[cfg(any(test, feature = "test-fixture"))]
pub(crate) fn builtins_floor() -> usize {
    const STEP: usize = 256;
    let (mut low, mut high) = (
        MINIMUM_HEAP_BYTES / STEP,
        crate::runtime::VM_MEMORY_BYTES_PER_RUN / STEP,
    );
    while low < high {
        let middle = low.saturating_add(high.saturating_sub(low) / 2);
        if installs(middle.saturating_mul(STEP), None::<&NoPackage>) {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    high.saturating_mul(STEP)
}

#[cfg(any(test, feature = "test-fixture"))]
struct NoPackage;

#[cfg(any(test, feature = "test-fixture"))]
impl barracuda_lua::Package for NoPackage {
    fn install(&self, _lua: &mut Lua) -> barracuda_lua::Result<()> {
        Ok(())
    }
}

/// Failure constructing a fixed-memory Lua test state.
#[cfg(any(test, feature = "test-fixture"))]
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

    use barracuda_vm_package_api::{
        LUA_PACKAGE_HEAP_BUDGET_BYTES, LUA_PACKAGE_INSTALL_PEAK_BYTES, MAX_LUA_PACKAGES,
    };

    use super::{VmMemoryPool, VmMemoryPoolError};
    use crate::runtime::{SCRIPT_HEAP_RESERVE_BYTES, VM_MEMORY_BYTES_PER_RUN};

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

    #[test]
    fn a_full_registry_leaves_the_script_its_reserve() {
        let packages = MAX_LUA_PACKAGES
            .saturating_mul(LUA_PACKAGE_HEAP_BUDGET_BYTES)
            .saturating_add(LUA_PACKAGE_INSTALL_PEAK_BYTES);
        let left = VM_MEMORY_BYTES_PER_RUN
            .saturating_sub(packages)
            .saturating_sub(SCRIPT_HEAP_RESERVE_BYTES);
        assert!(
            super::builtins_floor() <= left,
            "the built-ins need {} bytes, {left} remain beside a full registry and the script",
            super::builtins_floor()
        );
    }

    /// A package holding a string of the given length in its heap.
    struct Holding(usize);

    impl barracuda_lua::Package for Holding {
        fn install(&self, lua: &mut barracuda_lua::Lua) -> barracuda_lua::Result<()> {
            lua.load(&alloc::format!("held = string.rep('x', {})", self.0))
                .exec()
        }
    }

    #[test]
    fn the_package_budget_admits_small_packages_and_refuses_large_ones() {
        assert!(super::FixedMemoryLua::with_default_heap().is_ok());
        assert!(super::fits_package_budget(&Holding(1024)));
        assert!(!super::fits_package_budget(&Holding(
            LUA_PACKAGE_HEAP_BUDGET_BYTES.saturating_mul(2)
        )));
    }
}
