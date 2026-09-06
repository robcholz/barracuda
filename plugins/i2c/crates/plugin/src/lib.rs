//! Lua package for I2C values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use barracuda_board_hal::{ExposedIo, ResourceSet};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_async::i2c::I2c;

const MAX_TRANSFER_BYTES: usize = 64 * 1024;

/// Takes the concrete Board-exposed I2C set and registers the `i2c` package.
#[barracuda_plugin::macros::plugin]
pub struct I2cPlugin<I2cSet> {
    hardware: Option<I2cSet>,
}

impl<I2cSet> I2cPlugin<I2cSet> {
    /// Takes exclusive ownership of the I2C set from the complete HAL.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo<I2c = I2cSet>,
    {
        Self {
            hardware: context.hal.io.take_i2c(),
        }
    }
}

impl<I2cSet, const M: usize> Plugin<M> for I2cPlugin<I2cSet>
where
    I2cSet: ResourceSet + Send + 'static,
    I2cSet::Resource: I2c + Send,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(I2cPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedI2c<I2cSet> = Arc<Mutex<CriticalSectionRawMutex, I2cSet>>;

struct I2cPackage<I2cSet> {
    hardware: Option<SharedI2c<I2cSet>>,
    active: Arc<AtomicBool>,
}

impl<I2cSet> I2cPackage<I2cSet> {
    fn new(hardware: Option<I2cSet>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<I2cSet> Package for I2cPackage<I2cSet>
where
    I2cSet: ResourceSet + Send + 'static,
    I2cSet::Resource: I2c + Send,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let available_active = Arc::clone(&self.active);
        let read = self.hardware.clone();
        let read_active = Arc::clone(&self.active);
        let write = self.hardware.clone();
        let write_active = Arc::clone(&self.active);
        let write_read = self.hardware.clone();
        let write_read_active = Arc::clone(&self.active);

        lua.register_lib("i2c", move |package| {
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
            package.register_async(
                "read",
                move |(name, address, length): (String, i64, i64)| {
                    let hardware = read.clone();
                    let active = Arc::clone(&read_active);
                    async move {
                        let (address, length) = match bus_args(address, length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(i2c_read(hardware, active, name, address, length).await)
                    }
                },
            )?;
            package.register_async(
                "write",
                move |(name, address, bytes): (String, i64, Vec<u8>)| {
                    let hardware = write.clone();
                    let active = Arc::clone(&write_active);
                    async move {
                        let address = match parse_address(address) {
                            Ok(address) => address,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(i2c_write(hardware, active, name, address, bytes).await)
                    }
                },
            )?;
            package.register_async(
                "write_read",
                move |(name, address, bytes, read_length): (String, i64, Vec<u8>, i64)| {
                    let hardware = write_read.clone();
                    let active = Arc::clone(&write_read_active);
                    async move {
                        let (address, read_length) = match bus_args(address, read_length) {
                            Ok(values) => values,
                            Err(error) => return Some(Err(error)),
                        };
                        Some(
                            i2c_write_read(hardware, active, name, address, bytes, read_length)
                                .await,
                        )
                    }
                },
            )
        })
    }
}

impl<I2cSet> LuaPackage for I2cPackage<I2cSet>
where
    I2cSet: ResourceSet + Send + 'static,
    I2cSet::Resource: I2c + Send,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "i2c"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

async fn i2c_read<I2cSet>(
    hardware: Option<SharedI2c<I2cSet>>,
    active: Arc<AtomicBool>,
    name: String,
    address: u8,
    length: usize,
) -> Result<Vec<u8>>
where
    I2cSet: ResourceSet,
    I2cSet::Resource: I2c,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let mut bytes = vec![0; length];
    let hardware = hardware.ok_or_else(|| Error::runtime("I2C is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    let bus = named_bus(&mut *hardware, &name)?;
    bus.read(address, &mut bytes)
        .await
        .map_err(|error| bus_error(&name, error))?;
    Ok(bytes)
}

async fn i2c_write<I2cSet>(
    hardware: Option<SharedI2c<I2cSet>>,
    active: Arc<AtomicBool>,
    name: String,
    address: u8,
    bytes: Vec<u8>,
) -> Result<()>
where
    I2cSet: ResourceSet,
    I2cSet::Resource: I2c,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let hardware = hardware.ok_or_else(|| Error::runtime("I2C is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .write(address, &bytes)
        .await
        .map_err(|error| bus_error(&name, error))
}

async fn i2c_write_read<I2cSet>(
    hardware: Option<SharedI2c<I2cSet>>,
    active: Arc<AtomicBool>,
    name: String,
    address: u8,
    bytes: Vec<u8>,
    read_length: usize,
) -> Result<Vec<u8>>
where
    I2cSet: ResourceSet,
    I2cSet::Resource: I2c,
    <I2cSet::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let mut read = vec![0; read_length];
    let hardware = hardware.ok_or_else(|| Error::runtime("I2C is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    ensure_active(&active)?;
    named_bus(&mut *hardware, &name)?
        .write_read(address, &bytes, &mut read)
        .await
        .map_err(|error| bus_error(&name, error))?;
    Ok(read)
}

fn named_bus<'a, I2cSet>(hardware: &'a mut I2cSet, name: &str) -> Result<&'a mut I2cSet::Resource>
where
    I2cSet: ResourceSet,
{
    hardware
        .get_mut(name)
        .ok_or_else(|| Error::runtime(format!("I2C `{name}` is not exposed by this Board")))
}

fn bus_error(name: &str, error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("I2C `{name}` transaction failed: {error:?}"))
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("I2C package has been revoked"))
    }
}

fn bus_args(address: i64, length: i64) -> Result<(u8, usize)> {
    Ok((parse_address(address)?, parse_length(length)?))
}

fn parse_address(value: i64) -> Result<u8> {
    let address = u8::try_from(value)
        .map_err(|_| Error::runtime("I2C address must be an unsigned 7-bit value"))?;
    if address <= 0x7f {
        Ok(address)
    } else {
        Err(Error::runtime(
            "I2C address must be an unsigned 7-bit value",
        ))
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

    use alloc::sync::Arc;
    use barracuda_board_hal::NamedResources;
    use core::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use embedded_hal::i2c::{ErrorType, Operation};
    use futures_lite::future::block_on;

    use super::*;

    struct TestI2c;

    impl ErrorType for TestI2c {
        type Error = Infallible;
    }

    impl I2c for TestI2c {
        async fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> core::result::Result<(), Self::Error> {
            for operation in operations {
                if let Operation::Read(bytes) = operation {
                    bytes.fill(address);
                }
            }
            Ok(())
        }
    }

    struct SerializingI2c {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl ErrorType for SerializingI2c {
        type Error = Infallible;
    }

    impl I2c for SerializingI2c {
        async fn transaction(
            &mut self,
            _address: u8,
            _operations: &mut [Operation<'_>],
        ) -> core::result::Result<(), Self::Error> {
            let active_now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active_now, Ordering::SeqCst);
            futures_lite::future::yield_now().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn lua_uses_embedded_hal_async_i2c_values() {
        let package = I2cPackage::new(Some(NamedResources::new([("sensors", TestI2c)])));
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
    fn package_mutex_serializes_async_i2c_transactions() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let hardware = Arc::new(Mutex::new(NamedResources::new([(
            "bus",
            SerializingI2c {
                active: active.clone(),
                max_active: max_active.clone(),
            },
        )])));

        block_on(async {
            let active = Arc::new(AtomicBool::new(true));
            let first = i2c_write(
                Some(hardware.clone()),
                Arc::clone(&active),
                "bus".into(),
                1,
                Vec::new(),
            );
            let second = i2c_write(Some(hardware), active, "bus".into(), 2, Vec::new());
            let (first, second) = futures_lite::future::zip(first, second).await;
            first.expect("first transaction");
            second.expect("second transaction");
        });

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rejects_ten_bit_addresses_from_the_seven_bit_api() {
        assert!(parse_address(0x80).is_err());
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
        let package = I2cPackage::new(Some(NamedResources::new([("sensors", TestI2c)])));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install I2C package");
        package.revoke();

        let revoked: bool = block_on(
            lua.load(
                "local i2c = require('i2c')\n\
                 local value, err = i2c.read('sensors', 42, 1)\n\
                 return not i2c.available('sensors') and value == nil and type(err) == 'string'",
            )
            .eval_async(),
        )
        .expect("run revoked I2C script");
        assert!(revoked);
    }
}
