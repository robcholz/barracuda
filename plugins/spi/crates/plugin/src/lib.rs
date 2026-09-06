//! Lua package for SPI values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use barracuda_board_hal::{ExposedIo, ResourceSet};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_async::spi::SpiBus;

const MAX_TRANSFER_BYTES: usize = 64 * 1024;

/// Takes the concrete Board-exposed SPI set and registers the `spi` package.
#[barracuda_plugin_api::plugin]
pub struct SpiPlugin<SpiSet> {
    hardware: Option<SpiSet>,
}

impl<SpiSet> SpiPlugin<SpiSet> {
    /// Takes exclusive ownership of the SPI set from the complete HAL.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo<Spi = SpiSet>,
    {
        Self {
            hardware: context.hal.io.take_spi(),
        }
    }
}

impl<SpiSet, const M: usize> Plugin<M> for SpiPlugin<SpiSet>
where
    SpiSet: ResourceSet + Send + 'static,
    SpiSet::Resource: SpiBus + Send,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin_manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(SpiPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedSpi<SpiSet> = Arc<Mutex<CriticalSectionRawMutex, SpiSet>>;

struct SpiPackage<SpiSet> {
    hardware: Option<SharedSpi<SpiSet>>,
    active: Arc<AtomicBool>,
}

impl<SpiSet> SpiPackage<SpiSet> {
    fn new(hardware: Option<SpiSet>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<SpiSet> Package for SpiPackage<SpiSet>
where
    SpiSet: ResourceSet + Send + 'static,
    SpiSet::Resource: SpiBus + Send,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let available_active = Arc::clone(&self.active);
        let read = self.hardware.clone();
        let read_active = Arc::clone(&self.active);
        let write = self.hardware.clone();
        let write_active = Arc::clone(&self.active);
        let transfer = self.hardware.clone();
        let transfer_active = Arc::clone(&self.active);
        let transfer_in_place = self.hardware.clone();
        let transfer_in_place_active = Arc::clone(&self.active);

        lua.register_lib("spi", move |package| {
            package.register_async("available", move |name: String| {
                let hardware = available.clone();
                let active = Arc::clone(&available_active);
                async move {
                    if !active.load(Ordering::Acquire) {
                        return Some(Ok(false));
                    }
                    let exists = match hardware {
                        Some(hardware) => {
                            let hardware = hardware.lock().await;
                            active.load(Ordering::Acquire) && hardware.contains(&name)
                        }
                        None => false,
                    };
                    Some(Ok(exists))
                }
            })?;
            package.register_async("read", move |(name, length): (String, i64)| {
                let hardware = read.clone();
                let active = Arc::clone(&read_active);
                async move {
                    let length = match parse_length(length) {
                        Ok(length) => length,
                        Err(error) => return Some(Err(error)),
                    };
                    Some(spi_read(hardware, active, name, length).await)
                }
            })?;
            package.register_async("write", move |(name, bytes): (String, Vec<u8>)| {
                let hardware = write.clone();
                let active = Arc::clone(&write_active);
                async move { Some(spi_write(hardware, active, name, bytes).await) }
            })?;
            package.register_async(
                "transfer",
                move |(name, bytes, read_length): (String, Vec<u8>, i64)| {
                    let hardware = transfer.clone();
                    let active = Arc::clone(&transfer_active);
                    async move {
                        let read_length = match parse_length(read_length) {
                            Ok(length) => length,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(spi_transfer(hardware, active, name, bytes, read_length).await)
                    }
                },
            )?;
            package.register_async(
                "transfer_in_place",
                move |(name, bytes): (String, Vec<u8>)| {
                    let hardware = transfer_in_place.clone();
                    let active = Arc::clone(&transfer_in_place_active);
                    async move { Some(spi_transfer_in_place(hardware, active, name, bytes).await) }
                },
            )
        })
    }
}

impl<SpiSet> LuaPackage for SpiPackage<SpiSet>
where
    SpiSet: ResourceSet + Send + 'static,
    SpiSet::Resource: SpiBus + Send,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "spi"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

async fn spi_read<SpiSet>(
    hardware: Option<SharedSpi<SpiSet>>,
    active: Arc<AtomicBool>,
    name: String,
    length: usize,
) -> Result<Vec<u8>>
where
    SpiSet: ResourceSet,
    SpiSet::Resource: SpiBus,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    let mut bytes = vec![0; length];
    let hardware = hardware.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .read(&mut bytes)
        .await
        .map_err(|error| bus_error(&name, error))?;
    Ok(bytes)
}

async fn spi_write<SpiSet>(
    hardware: Option<SharedSpi<SpiSet>>,
    active: Arc<AtomicBool>,
    name: String,
    bytes: Vec<u8>,
) -> Result<()>
where
    SpiSet: ResourceSet,
    SpiSet::Resource: SpiBus,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    let hardware = hardware.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .write(&bytes)
        .await
        .map_err(|error| bus_error(&name, error))
}

async fn spi_transfer<SpiSet>(
    hardware: Option<SharedSpi<SpiSet>>,
    active: Arc<AtomicBool>,
    name: String,
    write: Vec<u8>,
    read_length: usize,
) -> Result<Vec<u8>>
where
    SpiSet: ResourceSet,
    SpiSet::Resource: SpiBus,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    let mut read = vec![0; read_length];
    let hardware = hardware.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .transfer(&mut read, &write)
        .await
        .map_err(|error| bus_error(&name, error))?;
    Ok(read)
}

async fn spi_transfer_in_place<SpiSet>(
    hardware: Option<SharedSpi<SpiSet>>,
    active: Arc<AtomicBool>,
    name: String,
    mut bytes: Vec<u8>,
) -> Result<Vec<u8>>
where
    SpiSet: ResourceSet,
    SpiSet::Resource: SpiBus,
    <SpiSet::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    let hardware = hardware.ok_or_else(|| Error::runtime("SPI is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .transfer_in_place(&mut bytes)
        .await
        .map_err(|error| bus_error(&name, error))?;
    Ok(bytes)
}

fn named_bus<'a, SpiSet>(hardware: &'a mut SpiSet, name: &str) -> Result<&'a mut SpiSet::Resource>
where
    SpiSet: ResourceSet,
{
    hardware
        .get_mut(name)
        .ok_or_else(|| Error::runtime(format!("SPI `{name}` is not exposed by this Board")))
}

fn bus_error(name: &str, error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("SPI `{name}` transaction failed: {error:?}"))
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("SPI package has been revoked"))
    }
}

fn parse_length(value: i64) -> Result<usize> {
    let length = usize::try_from(value)
        .map_err(|_| Error::runtime("transfer length must be non-negative"))?;
    if length <= MAX_TRANSFER_BYTES {
        Ok(length)
    } else {
        Err(Error::runtime(format!(
            "transfer length must not exceed {MAX_TRANSFER_BYTES} bytes"
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_board_hal::NamedResources;
    use core::convert::Infallible;
    use embedded_hal::spi::ErrorType;
    use futures_lite::future::block_on;

    use super::*;

    struct TestSpi;

    impl ErrorType for TestSpi {
        type Error = Infallible;
    }

    impl SpiBus for TestSpi {
        async fn read(&mut self, words: &mut [u8]) -> core::result::Result<(), Self::Error> {
            words.fill(0xA5);
            Ok(())
        }

        async fn write(&mut self, _words: &[u8]) -> core::result::Result<(), Self::Error> {
            Ok(())
        }

        async fn transfer(
            &mut self,
            read: &mut [u8],
            write: &[u8],
        ) -> core::result::Result<(), Self::Error> {
            read.fill(write.first().copied().unwrap_or_default());
            Ok(())
        }

        async fn transfer_in_place(
            &mut self,
            words: &mut [u8],
        ) -> core::result::Result<(), Self::Error> {
            words.reverse();
            Ok(())
        }

        async fn flush(&mut self) -> core::result::Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn lua_uses_embedded_hal_async_spi_values() {
        let package = SpiPackage::new(Some(NamedResources::new([("display-port", TestSpi)])));
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

    #[test]
    fn rejects_transfers_that_can_exhaust_the_device_heap() {
        assert_eq!(
            parse_length(MAX_TRANSFER_BYTES as i64),
            Ok(MAX_TRANSFER_BYTES)
        );
        assert!(parse_length(MAX_TRANSFER_BYTES as i64 + 1).is_err());
        assert!(parse_length(i64::MAX).is_err());
    }

    #[test]
    fn revocation_disables_callbacks_in_existing_lua_states() {
        let package = SpiPackage::new(Some(NamedResources::new([("display-port", TestSpi)])));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install SPI package");
        package.revoke();

        let revoked: bool = block_on(
            lua.load(
                "local spi = require('spi')\n\
                 local value, err = spi.read('display-port', 1)\n\
                 return not spi.available('display-port') and value == nil and type(err) == 'string'",
            )
            .eval_async(),
        )
        .expect("run revoked SPI script");
        assert!(revoked);
    }
}
