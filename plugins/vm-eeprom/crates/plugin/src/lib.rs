//! Lua package for the Board's primary built-in EEPROM.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use barracuda_board_hal::eeprom::{Eeprom, EepromGeometry, EepromPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's primary EEPROM and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct EepromPlugin<Device> {
    memory: Option<Device>,
}

impl<Device> EepromPlugin<Device> {
    /// Takes the built-in EEPROM from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: EepromPeripheral<Eeprom = Device>,
    {
        Self {
            memory: config::peripherals::EEPROM_DEVICE
                .and_then(|instance| context.hal.peripherals.take_eeprom(instance)),
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
            .register(EepromPackage::new(self.memory.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The memory while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The memory taken out of its slot for one call; it goes back when the
/// call ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("eeprom package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime("built-in EEPROM is unavailable or busy")),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in EEPROM is unavailable or busy"))
    }
}

impl<Device> Drop for Lent<Device> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            critical_section::with(|section| {
                self.home.borrow(section).replace(Some(device));
            });
        }
    }
}

struct EepromPackage<Device> {
    /// The memory's geometry, when the Board has one.
    geometry: Option<EepromGeometry>,
    memory: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device: Eeprom> EepromPackage<Device> {
    fn new(memory: Option<Device>) -> Self {
        Self {
            geometry: memory.as_ref().map(Eeprom::geometry),
            memory: Arc::new(critical_section::Mutex::new(RefCell::new(memory))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Checks a Lua range against the memory and returns its offset.
fn range(geometry: Option<EepromGeometry>, offset: i64, length: usize) -> Result<u32> {
    let geometry = geometry.ok_or_else(|| Error::runtime("built-in EEPROM is unavailable"))?;
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
        let geometry = self.geometry;
        let available = (Arc::clone(&self.memory), Arc::clone(&self.active));
        let read = (Arc::clone(&self.memory), Arc::clone(&self.active));
        let write = (Arc::clone(&self.memory), Arc::clone(&self.active));
        lua.register_lib("eeprom", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register("size", move |(): ()| {
                Some(
                    geometry
                        .map(|geometry| i64::from(geometry.capacity()))
                        .ok_or_else(|| Error::runtime("built-in EEPROM is unavailable")),
                )
            })?;
            package.register("page_size", move |(): ()| {
                Some(
                    geometry
                        .map(|geometry| i64::from(geometry.page_size()))
                        .ok_or_else(|| Error::runtime("built-in EEPROM is unavailable")),
                )
            })?;
            package.register_async("read", move |(offset, length): (i64, i64)| {
                let (slot, active) = (Arc::clone(&read.0), Arc::clone(&read.1));
                let parsed = usize::try_from(length)
                    .map_err(|_| Error::runtime("a length is a byte count, 0 or more"))
                    .and_then(|length| Ok((range(geometry, offset, length)?, length)));
                async move {
                    Some(
                        async {
                            let (offset, length) = parsed?;
                            let mut bytes = Bytes::zeroed(length)?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .read(offset, &mut bytes)
                                .await
                                .map_err(memory_error)?;
                            Ok(bytes)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("write", move |(offset, data): (i64, Bytes)| {
                let (slot, active) = (Arc::clone(&write.0), Arc::clone(&write.1));
                let parsed = range(geometry, offset, data.len());
                async move {
                    Some(
                        async {
                            let offset = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .write(offset, &data)
                                .await
                                .map_err(memory_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })
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
        self.geometry
            .is_some()
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "断电保存的小块数据",
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
        let package = EepromPackage::new(Some(TestMemory {
            cells: Arc::clone(&cells),
        }));
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
    fn a_board_without_eeprom_reports_unavailable() {
        let package = EepromPackage::<TestMemory>::new(None);
        assert!(run(
            &package,
            "local eeprom = require('eeprom')\n\
             local ok, err = eeprom.read(0, 1)\n\
             return not eeprom.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = EepromPackage::new(Some(TestMemory {
            cells: Arc::new(critical_section::Mutex::new(RefCell::new([0u8; 32]))),
        }));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
