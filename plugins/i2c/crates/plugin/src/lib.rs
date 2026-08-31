//! Lua package for I2C controllers explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use barracuda_board_hal::I2cService;
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_plugin::PLUGIN_ID as VM_PLUGIN_ID;

/// Stable identity of the I2C Plugin.
pub const PLUGIN_ID: &str = "i2c";

/// Registers the `i2c` Lua package with the VM Plugin.
pub struct I2cPlugin {
    service: Option<Arc<dyn I2cService>>,
}

impl I2cPlugin {
    /// Creates the Plugin from Board hardware resources.
    #[must_use]
    pub fn new(context: &PluginContext) -> Self {
        Self {
            service: context.hardware_services.i2c(),
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
            .register(I2cPackage {
                service: self.service.clone(),
            })
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct I2cPackage {
    service: Option<Arc<dyn I2cService>>,
}

impl Package for I2cPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.service.clone();
        let read = self.service.clone();
        let write = self.service.clone();
        let write_read = self.service.clone();

        lua.register_lib("i2c", move |package| {
            package.register("available", move |name: String| {
                Some(Ok(available
                    .as_ref()
                    .is_some_and(|service| service.contains(&name))))
            })?;
            package.register_async(
                "read",
                move |(name, address, length): (String, i64, i64)| {
                    let service = read.clone();
                    async move {
                        let (address, length) = match bus_args(address, length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(call_i2c(service, |service| service.read(name, address, length)).await)
                    }
                },
            )?;
            package.register_async(
                "write",
                move |(name, address, bytes): (String, i64, Vec<u8>)| {
                    let service = write.clone();
                    async move {
                        let address = match parse_address(address) {
                            Ok(address) => address,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(call_i2c(service, |service| service.write(name, address, bytes)).await)
                    }
                },
            )?;
            package.register_async(
                "write_read",
                move |(name, address, bytes, read_length): (String, i64, Vec<u8>, i64)| {
                    let service = write_read.clone();
                    async move {
                        let (address, read_length) = match bus_args(address, read_length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_i2c(service, |service| {
                                service.write_read(name, address, bytes, read_length)
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
    service: Option<Arc<dyn I2cService>>,
    operation: impl for<'a> FnOnce(&'a dyn I2cService) -> barracuda_board_hal::ServiceFuture<'a, T>,
) -> Result<T> {
    let service = service.ok_or_else(|| Error::runtime("I2C is not exposed by this Board"))?;
    operation(service.as_ref())
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
    use barracuda_board_hal::ServiceFuture;
    use futures_lite::future::block_on;

    use super::*;

    struct TestI2c;

    impl I2cService for TestI2c {
        fn contains(&self, name: &str) -> bool {
            name == "sensors"
        }

        fn read(&self, _name: String, address: u16, length: usize) -> ServiceFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![address as u8; length]) })
        }

        fn write(&self, _name: String, _address: u16, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn write_read(
            &self,
            _name: String,
            address: u16,
            _bytes: Vec<u8>,
            read_length: usize,
        ) -> ServiceFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![address as u8; read_length]) })
        }
    }

    #[test]
    fn lua_uses_binary_strings_for_i2c_transactions() {
        let package = I2cPackage {
            service: Some(Arc::new(TestI2c)),
        };
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
}
