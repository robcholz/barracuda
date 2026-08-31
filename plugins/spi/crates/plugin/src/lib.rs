//! Lua package for SPI buses explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use barracuda_board_hal::SpiService;
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_plugin::PLUGIN_ID as VM_PLUGIN_ID;

/// Stable identity of the SPI Plugin.
pub const PLUGIN_ID: &str = "spi";

/// Registers the `spi` Lua package with the VM Plugin.
pub struct SpiPlugin {
    service: Option<Arc<dyn SpiService>>,
}

impl SpiPlugin {
    /// Creates the Plugin from Board hardware resources.
    #[must_use]
    pub fn new(context: &PluginContext) -> Self {
        Self {
            service: context.hardware_services.spi(),
        }
    }
}

impl<const M: usize> Plugin<M> for SpiPlugin {
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
            .register(SpiPackage {
                service: self.service.clone(),
            })
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct SpiPackage {
    service: Option<Arc<dyn SpiService>>,
}

impl Package for SpiPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.service.clone();
        let read = self.service.clone();
        let write = self.service.clone();
        let transfer = self.service.clone();
        let transfer_in_place = self.service.clone();

        lua.register_lib("spi", move |package| {
            package.register("available", move |name: String| {
                Some(Ok(available
                    .as_ref()
                    .is_some_and(|service| service.contains(&name))))
            })?;
            package.register_async("read", move |(name, length): (String, i64)| {
                let service = read.clone();
                async move {
                    let length = match parse_length(length) {
                        Ok(length) => length,
                        Err(error) => return Some(Err(error)),
                    };
                    Some(call_spi(service, |service| service.read(name, length)).await)
                }
            })?;
            package.register_async("write", move |(name, bytes): (String, Vec<u8>)| {
                let service = write.clone();
                async move { Some(call_spi(service, |service| service.write(name, bytes)).await) }
            })?;
            package.register_async(
                "transfer",
                move |(name, bytes, read_length): (String, Vec<u8>, i64)| {
                    let service = transfer.clone();
                    async move {
                        let read_length = match parse_length(read_length) {
                            Ok(length) => length,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            call_spi(service, |service| {
                                service.transfer(name, bytes, read_length)
                            })
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "transfer_in_place",
                move |(name, bytes): (String, Vec<u8>)| {
                    let service = transfer_in_place.clone();
                    async move {
                        Some(
                            call_spi(service, |service| service.transfer_in_place(name, bytes))
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
    service: Option<Arc<dyn SpiService>>,
    operation: impl for<'a> FnOnce(&'a dyn SpiService) -> barracuda_board_hal::ServiceFuture<'a, T>,
) -> Result<T> {
    let service = service.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    operation(service.as_ref())
        .await
        .map_err(|error| Error::runtime(error.message()))
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

    struct TestSpi;

    impl SpiService for TestSpi {
        fn contains(&self, name: &str) -> bool {
            name == "display-port"
        }

        fn read(&self, _name: String, length: usize) -> ServiceFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(vec![0xA5; length]) })
        }

        fn write(&self, _name: String, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn transfer(
            &self,
            _name: String,
            write: Vec<u8>,
            read_length: usize,
        ) -> ServiceFuture<'_, Vec<u8>> {
            Box::pin(async move {
                let fill = write.first().copied().unwrap_or_default();
                Ok(vec![fill; read_length])
            })
        }

        fn transfer_in_place(
            &self,
            _name: String,
            mut bytes: Vec<u8>,
        ) -> ServiceFuture<'_, Vec<u8>> {
            Box::pin(async move {
                bytes.reverse();
                Ok(bytes)
            })
        }
    }

    #[test]
    fn lua_uses_binary_strings_for_spi_transactions() {
        let package = SpiPackage {
            service: Some(Arc::new(TestSpi)),
        };
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
