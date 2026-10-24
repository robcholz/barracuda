//! Lua package for the Board's built-in EEPROMs.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::eeprom::{Eeprom, EepromGeometry, EepromPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, Package, Result,
    Trailing, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's EEPROMs and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct EepromPlugin<Device> {
    memories: Vec<(&'static str, Device)>,
}

impl<Device> EepromPlugin<Device> {
    /// Takes the built-in EEPROMs from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: EepromPeripheral<Eeprom = Device>,
    {
        Self {
            memories: config::peripherals::EEPROM_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_eeprom(*instance)
                        .map(|memory| (*name, memory))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for EepromPlugin<Device>
where
    Device: Eeprom + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(EepromPackage::new(core::mem::take(&mut self.memories)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Lends the memory `name`, or the primary one, to one call; it goes back
/// when the call ends or is cancelled.
fn lend<Device>(
    memories: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
) -> Result<LentDevice<Device>> {
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("eeprom package has been revoked"));
    }
    memories.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("EEPROM `{name}` is unavailable or busy"),
            None => String::from("built-in EEPROM is unavailable or busy"),
        })
    })
}

/// Each memory's geometry by Board name, the primary one first.
type Geometries = Arc<Vec<(&'static str, EepromGeometry)>>;

struct EepromPackage<Device> {
    geometries: Geometries,
    memories: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device: Eeprom> EepromPackage<Device> {
    fn new(memories: Vec<(&'static str, Device)>) -> Self {
        Self {
            geometries: Arc::new(
                memories
                    .iter()
                    .map(|(name, memory)| (*name, memory.geometry()))
                    .collect(),
            ),
            memories: BoardDevices::new(memories),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// The geometry of the memory `name`, or of the primary one.
fn geometry(geometries: &Geometries, name: Option<&str>) -> Result<EepromGeometry> {
    let found = match name {
        None => geometries.first(),
        Some(name) => geometries.iter().find(|(memory, _)| *memory == name),
    };
    found.map(|(_, geometry)| *geometry).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("EEPROM `{name}` is unavailable"),
            None => String::from("built-in EEPROM is unavailable"),
        })
    })
}

/// Checks a Lua range against the memory and returns its offset.
fn range(geometry: EepromGeometry, offset: i64, length: usize) -> Result<u32> {
    let offset = u32::try_from(offset)
        .map_err(|_| Error::runtime("an offset is a byte position, 0 or more"))?;
    if geometry.contains(offset, length) {
        Ok(offset)
    } else {
        Err(Error::runtime(alloc::format!(
            "the range lies past the end of the {}-byte EEPROM",
            geometry.capacity()
        )))
    }
}

fn memory_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("EEPROM operation failed: {error:?}"))
}

impl<Device> Package for EepromPackage<Device>
where
    Device: Eeprom + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.memories.clone(), Arc::clone(&self.active));
        let list = self.memories.clone();
        let size = Arc::clone(&self.geometries);
        let page_size = Arc::clone(&self.geometries);
        let read = (
            self.memories.clone(),
            Arc::clone(&self.active),
            Arc::clone(&self.geometries),
        );
        let write = (
            self.memories.clone(),
            Arc::clone(&self.active),
            Arc::clone(&self.geometries),
        );
        lua.register_lib("eeprom", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (memories, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && memories.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("size", move |name: Variadic<String>| {
                Some(
                    geometry(&size, name.first().map(String::as_str))
                        .map(|geometry| i64::from(geometry.capacity())),
                )
            })?;
            package.register("page_size", move |name: Variadic<String>| {
                Some(
                    geometry(&page_size, name.first().map(String::as_str))
                        .map(|geometry| i64::from(geometry.page_size())),
                )
            })?;
            package.register_async(
                "read",
                move |Trailing((offset, length), name): Trailing<(i64, i64), String>| {
                    let (memories, active) = (read.0.clone(), Arc::clone(&read.1));
                    let parsed = usize::try_from(length)
                        .map_err(|_| Error::runtime("a length is a byte count, 0 or more"))
                        .and_then(|length| {
                            let geometry = geometry(&read.2, name.as_deref())?;
                            Ok((range(geometry, offset, length)?, length))
                        });
                    async move {
                        Some(
                            async {
                                let (offset, length) = parsed?;
                                let mut bytes = Bytes::zeroed(length)?;
                                let mut lent = lend(&memories, &active, name.as_deref())?;
                                lent.read(offset, &mut bytes).await.map_err(memory_error)?;
                                Ok(bytes)
                            }
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "write",
                move |Trailing((offset, data), name): Trailing<(i64, Bytes), String>| {
                    let (memories, active) = (write.0.clone(), Arc::clone(&write.1));
                    let parsed = geometry(&write.2, name.as_deref())
                        .and_then(|geometry| range(geometry, offset, data.len()));
                    async move {
                        Some(
                            async {
                                let offset = parsed?;
                                let mut lent = lend(&memories, &active, name.as_deref())?;
                                lent.write(offset, &data).await.map_err(memory_error)?;
                                Ok(true)
                            }
                            .await,
                        )
                    }
                },
            )
        })
    }
}

impl<Device> LuaPackage for EepromPackage<Device>
where
    Device: Eeprom + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "eeprom"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.memories.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "Keep small data through power loss",
        })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::cell::RefCell;
    use core::convert::Infallible;

    /// A 32-byte memory in 8-byte pages.
    struct TestMemory {
        cells: Arc<critical_section::Mutex<RefCell<[u8; 32]>>>,
    }

    impl Eeprom for TestMemory {
        type Error = Infallible;

        fn geometry(&self) -> EepromGeometry {
            EepromGeometry::new(32, 8)
        }

        async fn read(
            &mut self,
            offset: u32,
            buffer: &mut [u8],
        ) -> core::result::Result<(), Infallible> {
            critical_section::with(|section| {
                let cells = self.cells.borrow(section).borrow();
                let start = usize::try_from(offset).expect("offset");
                buffer.copy_from_slice(&cells[start..start + buffer.len()]);
            });
            Ok(())
        }

        async fn write(
            &mut self,
            offset: u32,
            data: &[u8],
        ) -> core::result::Result<(), Infallible> {
            critical_section::with(|section| {
                let mut cells = self.cells.borrow(section).borrow_mut();
                let start = usize::try_from(offset).expect("offset");
                cells[start..start + data.len()].copy_from_slice(data);
            });
            Ok(())
        }
    }

    fn run<Device>(package: &EepromPackage<Device>, script: &str) -> bool
    where
        Device: Eeprom + Send + 'static,
        Device::Error: core::fmt::Debug,
    {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_writes_and_reads_bytes_within_range() {
        let cells = Arc::new(critical_section::Mutex::new(RefCell::new([0u8; 32])));
        let package = EepromPackage::new(std::vec![(
            "main",
            TestMemory {
                cells: Arc::clone(&cells),
            }
        )]);
        assert!(run(
            &package,
            "local eeprom = require('eeprom')\n\
             assert(eeprom.available() and eeprom.size() == 32 and eeprom.page_size() == 8)\n\
             assert(eeprom.write(6, 'hello\\0world'))\n\
             assert(eeprom.read(6, 11) == 'hello\\0world')\n\
             local ok, err = eeprom.write(30, 'abc')\n\
             assert(ok == nil and err:find('past the end') ~= nil)\n\
             ok, err = eeprom.read(-1, 1)\n\
             return ok == nil and type(err) == 'string'",
        ));
        let stored = critical_section::with(|section| *cells.borrow(section).borrow());
        assert_eq!(&stored[6..17], b"hello\0world");
    }

    #[test]
    fn lua_names_a_memory_and_defaults_to_the_primary() {
        let main = Arc::new(critical_section::Mutex::new(RefCell::new([0u8; 32])));
        let spare = Arc::new(critical_section::Mutex::new(RefCell::new([0u8; 32])));
        let package = EepromPackage::new(std::vec![
            (
                "main",
                TestMemory {
                    cells: Arc::clone(&main)
                }
            ),
            (
                "spare",
                TestMemory {
                    cells: Arc::clone(&spare)
                }
            ),
        ]);
        assert!(run(
            &package,
            "local eeprom = require('eeprom')\n\
             local names = eeprom.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'spare')\n\
             assert(eeprom.size('spare') == 32)\n\
             assert(eeprom.write(0, 'a'))\n\
             assert(eeprom.write(0, 'b', 'spare'))\n\
             assert(eeprom.read(0, 1, 'spare') == 'b' and eeprom.read(0, 1) == 'a')\n\
             local ok, err = eeprom.read(0, 1, 'missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
        let first = |cells: &Arc<critical_section::Mutex<RefCell<[u8; 32]>>>| {
            critical_section::with(|section| cells.borrow(section).borrow()[0])
        };
        assert_eq!((first(&main), first(&spare)), (b'a', b'b'));
    }

    #[test]
    fn a_board_without_eeprom_reports_unavailable() {
        let package = EepromPackage::<TestMemory>::new(Vec::new());
        assert!(run(
            &package,
            "local eeprom = require('eeprom')\n\
             local ok, err = eeprom.read(0, 1)\n\
             return not eeprom.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = EepromPackage::new(std::vec![(
            "main",
            TestMemory {
                cells: Arc::new(critical_section::Mutex::new(RefCell::new([0u8; 32]))),
            }
        )]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
