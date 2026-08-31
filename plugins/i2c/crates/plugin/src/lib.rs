//! Lua package for I2C values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::{LuaHardwareFuture, LuaI2cHardware, LuaIo, PluginContext};
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_plugin::PLUGIN_ID as VM_PLUGIN_ID;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

/// Stable identity of the I2C Plugin.
pub const PLUGIN_ID: &str = "i2c";

/// Takes the Board-exposed I2C value and registers the `i2c` Lua package.
pub struct I2cPlugin {
    hardware: Option<Box<dyn LuaI2cHardware>>,
}

impl I2cPlugin {
    /// Takes exclusive ownership of the I2C value from shared construction resources.
    #[must_use]
    pub fn new<Builtins, Io: LuaIo>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            hardware: context.hal.io.take_i2c(),
        }
    }
}

impl<const M: usize> Plugin<M> for I2cPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[VM_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(VM_PLUGIN_ID)?;
        let registration = registry
            .register(I2cPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedI2c = Arc<Mutex<CriticalSectionRawMutex, Box<dyn LuaI2cHardware>>>;

struct I2cPackage {
    hardware: Option<SharedI2c>,
}

impl I2cPackage {
    fn new(hardware: Option<Box<dyn LuaI2cHardware>>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
        }
    }
}

impl Package for I2cPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let read = self.hardware.clone();
        let write = self.hardware.clone();
        let write_read = self.hardware.clone();

        lua.register_lib("i2c", move |package| {
            package.register_async("available", move |name: String| {
                let hardware = available.clone();
                async move {
                    let exists = match hardware {
                        Some(hardware) => hardware.lock().await.contains(&name),
                        None => false,
                    };
                    Some(Ok(exists))
                }
            })?;
            package.register_async(
                "read",
                move |(name, address, length): (String, i64, i64)| {
                    let hardware = read.clone();
                    async move {
                        let (address, length) = match bus_args(address, length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_i2c(hardware, |hardware| hardware.read(name, address, length))
                                .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "write",
                move |(name, address, bytes): (String, i64, Vec<u8>)| {
                    let hardware = write.clone();
                    async move {
                        let address = match parse_address(address) {
                            Ok(address) => address,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_i2c(hardware, |hardware| hardware.write(name, address, bytes))
                                .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "write_read",
                move |(name, address, bytes, read_length): (String, i64, Vec<u8>, i64)| {
                    let hardware = write_read.clone();
                    async move {
                        let (address, read_length) = match bus_args(address, read_length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_i2c(hardware, |hardware| {
                                hardware.write_read(name, address, bytes, read_length)
                            })
                            .await,
                        )
                    }
                },
            )
        })
    }
}

impl LuaPackage for I2cPackage {
    fn name(&self) -> &'static str {
        "i2c"
    }
}

async fn call_i2c<T>(
    hardware: Option<SharedI2c>,
    operation: impl for<'a> FnOnce(&'a mut dyn LuaI2cHardware) -> LuaHardwareFuture<'a, T>,
) -> Result<T> {
    let hardware = hardware.ok_or_else(|| Error::runtime("I2C is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    operation(hardware.as_mut())
        .await
        .map_err(|error| Error::runtime(error.message()))
}

fn bus_args(address: i64, length: i64) -> Result<(u16, usize)> {
    Ok((parse_address(address)?, parse_length(length)?))
}

fn parse_address(value: i64) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| Error::runtime("I2C address must fit in an unsigned 16-bit value"))
}

fn parse_length(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::runtime("transfer length must be non-negative"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{boxed::Box, sync::Arc, vec};
    use barracuda_plugin_api::LuaHardwareFuture;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use futures_lite::future::block_on;

    use super::*;

    struct TestI2c;

    impl LuaI2cHardware for TestI2c {
        fn contains(&self, name: &str) -> bool {
            name == "sensors"
        }

        fn read(
            &mut self,
            _name: String,
            address: u16,
            length: usize,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![address as u8; length]) })
        }

        fn write(
            &mut self,
            _name: String,
            _address: u16,
            _bytes: Vec<u8>,
        ) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn write_read(
            &mut self,
            _name: String,
            address: u16,
            _bytes: Vec<u8>,
            read_length: usize,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![address as u8; read_length]) })
        }
    }

    struct SerializingI2c {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl LuaI2cHardware for SerializingI2c {
        fn contains(&self, _name: &str) -> bool {
            true
        }

        fn read(
            &mut self,
            _name: String,
            _address: u16,
            _length: usize,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn write(
            &mut self,
            _name: String,
            _address: u16,
            _bytes: Vec<u8>,
        ) -> LuaHardwareFuture<'_, ()> {
            let active = self.active.clone();
            let max_active = self.max_active.clone();
            Box::pin(async move {
                let active_now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(active_now, Ordering::SeqCst);
                futures_lite::future::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn write_read(
            &mut self,
            _name: String,
            _address: u16,
            _bytes: Vec<u8>,
            _read_length: usize,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[test]
    fn lua_uses_binary_strings_for_i2c_transactions() {
        let package = I2cPackage::new(Some(Box::new(TestI2c)));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install I2C package");

        let valid: bool = block_on(
            lua.load(
                "local i2c = require('i2c')\n\
                 i2c.write('sensors', 42, 'xy')\n\
                 local data = i2c.write_read('sensors', 42, 'z', 3)\n\
                 return i2c.available('sensors') and data == '***'",
            )
            .eval_async(),
        )
        .expect("run I2C script");
        assert!(valid);
    }

    #[test]
    fn lua_layer_mutex_serializes_mutable_hardware_operations() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let hardware: SharedI2c = Arc::new(Mutex::new(Box::new(SerializingI2c {
            active: active.clone(),
            max_active: max_active.clone(),
        })));

        block_on(async {
            let first = call_i2c(Some(hardware.clone()), |hardware| {
                hardware.write("bus".into(), 1, Vec::new())
            });
            let second = call_i2c(Some(hardware), |hardware| {
                hardware.write("bus".into(), 2, Vec::new())
            });
            let (first, second) = futures_lite::future::zip(first, second).await;
            first.expect("first transaction");
            second.expect("second transaction");
        });

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
