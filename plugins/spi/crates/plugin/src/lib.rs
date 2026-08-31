//! Lua package for SPI values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::{LuaHardwareFuture, LuaIo, LuaSpiHardware, PluginContext};
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

/// Takes the Board-exposed SPI value and registers the `spi` Lua package.
pub struct SpiPlugin {
    hardware: Option<Box<dyn LuaSpiHardware>>,
}

impl SpiPlugin {
    /// Takes exclusive ownership of the SPI value from shared construction resources.
    #[must_use]
    pub fn new<Builtins, Io: LuaIo>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            hardware: context.hal.io.take_spi(),
        }
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for SpiPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(<Self as Plugin<M>>::DEPENDS_ON[0])?;
        let registration = registry
            .register(SpiPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedSpi = Arc<Mutex<CriticalSectionRawMutex, Box<dyn LuaSpiHardware>>>;

struct SpiPackage {
    hardware: Option<SharedSpi>,
}

impl SpiPackage {
    fn new(hardware: Option<Box<dyn LuaSpiHardware>>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
        }
    }
}

impl Package for SpiPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let read = self.hardware.clone();
        let write = self.hardware.clone();
        let transfer = self.hardware.clone();
        let transfer_in_place = self.hardware.clone();

        lua.register_lib("spi", move |package| {
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
            package.register_async("read", move |(name, length): (String, i64)| {
                let hardware = read.clone();
                async move {
                    let length = match parse_length(length) {
                        Ok(length) => length,
                        Err(error) => return Some(Err(error)),
                    };
                    Some(call_spi(hardware, |hardware| hardware.read(name, length)).await)
                }
            })?;
            package.register_async("write", move |(name, bytes): (String, Vec<u8>)| {
                let hardware = write.clone();
                async move {
                    Some(call_spi(hardware, |hardware| hardware.write(name, bytes)).await)
                }
            })?;
            package.register_async(
                "transfer",
                move |(name, bytes, read_length): (String, Vec<u8>, i64)| {
                    let hardware = transfer.clone();
                    async move {
                        let read_length = match parse_length(read_length) {
                            Ok(length) => length,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_spi(hardware, |hardware| {
                                hardware.transfer(name, bytes, read_length)
                            })
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "transfer_in_place",
                move |(name, bytes): (String, Vec<u8>)| {
                    let hardware = transfer_in_place.clone();
                    async move {
                        Some(
                            call_spi(hardware, |hardware| hardware.transfer_in_place(name, bytes))
                                .await,
                        )
                    }
                },
            )
        })
    }
}

impl LuaPackage for SpiPackage {
    fn name(&self) -> &'static str {
        "spi"
    }
}

async fn call_spi<T>(
    hardware: Option<SharedSpi>,
    operation: impl for<'a> FnOnce(&'a mut dyn LuaSpiHardware) -> LuaHardwareFuture<'a, T>,
) -> Result<T> {
    let hardware = hardware.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    operation(hardware.as_mut())
        .await
        .map_err(|error| Error::runtime(error.message()))
}

fn parse_length(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::runtime("transfer length must be non-negative"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{boxed::Box, vec};
    use barracuda_plugin_api::LuaHardwareFuture;
    use futures_lite::future::block_on;

    use super::*;

    struct TestSpi;

    impl LuaSpiHardware for TestSpi {
        fn contains(&self, name: &str) -> bool {
            name == "display-port"
        }

        fn read(&mut self, _name: String, length: usize) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![0xA5; length]) })
        }

        fn write(&mut self, _name: String, _bytes: Vec<u8>) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn transfer(
            &mut self,
            _name: String,
            write: Vec<u8>,
            read_length: usize,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async move {
                let fill = write.first().copied().unwrap_or_default();
                Ok(vec![fill; read_length])
            })
        }

        fn transfer_in_place(
            &mut self,
            _name: String,
            mut bytes: Vec<u8>,
        ) -> LuaHardwareFuture<'_, Vec<u8>> {
            Box::pin(async move {
                bytes.reverse();
                Ok(bytes)
            })
        }
    }

    #[test]
    fn lua_uses_binary_strings_for_spi_transactions() {
        let package = SpiPackage::new(Some(Box::new(TestSpi)));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install SPI package");

        let valid: bool = block_on(
            lua.load(
                "local spi = require('spi')\n\
                 spi.write('display-port', '12')\n\
                 local data = spi.transfer_in_place('display-port', '34')\n\
                 return spi.available('display-port') and data == '43'",
            )
            .eval_async(),
        )
        .expect("run SPI script");
        assert!(valid);
    }
}
