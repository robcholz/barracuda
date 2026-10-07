//! Lua package for runtime I2C handles over Board-exposed resources.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, string::ToString};
use barracuda_board_hal::{ExposedIo, I2cProvider, I2cRequest};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

const MAX_TRANSFER_BYTES: usize = 64 * 1024;

/// Shares the unified exposed-I/O owner with the `i2c` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct I2cPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> I2cPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + I2cProvider,
    {
        Self {
            io: Arc::clone(&context.hal.exposed_io),
        }
    }
}

impl<Io> Plugin for I2cPlugin<Io>
where
    Io: ExposedIo + I2cProvider,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
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
            .register(I2cPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct I2cPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> I2cPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for I2cPackage<Io>
where
    Io: I2cProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let io = Arc::clone(&self.io);
        let active = Arc::clone(&self.active);
        lua.register_lib("i2c", move |package| {
            package.register_with(
                "open",
                move |lua, (scl, sda, frequency_hz): (String, String, i64)| {
                    if !active.load(Ordering::Acquire) {
                        return Some(Err(Error::runtime("I2C package has been revoked")));
                    }
                    let result = (|| {
                        if scl == sda {
                            return Err(Error::runtime("I2C SCL and SDA must use different pins"));
                        }
                        let frequency_hz = positive_u32(frequency_hz, "I2C frequency")?;
                        let bus = io
                            .open_i2c(I2cRequest {
                                scl: &scl,
                                sda: &sda,
                                frequency_hz,
                            })
                            .map_err(|error| Error::runtime(error.to_string()))?;
                        let label = format!("SCL `{scl}`, SDA `{sda}`");
                        lua.create_userdata(I2cHandle::new(label, bus, Arc::clone(&active)))
                    })();
                    Some(result)
                },
            )
        })
    }
}

impl<Io> LuaPackage for I2cPackage<Io>
where
    Io: I2cProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "i2c"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct I2cHandle<Bus> {
    label: String,
    bus: Option<Bus>,
    active: Arc<AtomicBool>,
}

impl<Bus> I2cHandle<Bus> {
    fn new(label: String, bus: Bus, active: Arc<AtomicBool>) -> Self {
        Self {
            label,
            bus: Some(bus),
            active,
        }
    }

    fn close(&mut self) {
        self.bus.take();
    }
}

impl<Bus> UserData for I2cHandle<Bus>
where
    Bus: I2c + Send + 'static,
    <Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method("read", |handle, (address, length): (i64, i64)| async move {
            Some(i2c_read(handle, address, length).await)
        });
        methods.add_async_method(
            "write",
            |handle, (address, bytes): (i64, Bytes)| async move {
                Some(i2c_write(handle, address, bytes).await)
            },
        );
        methods.add_async_method(
            "write_read",
            |handle, (address, bytes, length): (i64, Bytes, i64)| async move {
                Some(i2c_write_read(handle, address, bytes, length).await)
            },
        );
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.bus.is_some()
            ))
        });
        methods.add_method_mut("close", |handle, (): ()| {
            handle.close();
            None::<Result<()>>
        });
        methods.add_meta_method_mut(MetaMethod::Close, |handle, _error: Option<String>| {
            handle.close();
            None::<Result<()>>
        });
    }
}

async fn i2c_read<Bus>(
    handle: UserDataHandle<I2cHandle<Bus>>,
    address: i64,
    length: i64,
) -> Result<Bytes>
where
    Bus: I2c + Send + 'static,
    <Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let address = parse_address(address)?;
    let length = parse_length(length)?;
    let mut bytes = Bytes::zeroed(length)?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("I2C handle is closed"))?
        .read(address, &mut bytes)
        .await
        .map_err(|error| bus_error(&label, error))?;
    Ok(bytes)
}

async fn i2c_write<Bus>(
    handle: UserDataHandle<I2cHandle<Bus>>,
    address: i64,
    bytes: Bytes,
) -> Result<()>
where
    Bus: I2c + Send + 'static,
    <Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let address = parse_address(address)?;
    check_length(bytes.len())?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("I2C handle is closed"))?
        .write(address, &bytes)
        .await
        .map_err(|error| bus_error(&label, error))
}

async fn i2c_write_read<Bus>(
    handle: UserDataHandle<I2cHandle<Bus>>,
    address: i64,
    bytes: Bytes,
    length: i64,
) -> Result<Bytes>
where
    Bus: I2c + Send + 'static,
    <Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    let address = parse_address(address)?;
    check_length(bytes.len())?;
    let length = parse_length(length)?;
    let mut read = Bytes::zeroed(length)?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("I2C handle is closed"))?
        .write_read(address, &bytes, &mut read)
        .await
        .map_err(|error| bus_error(&label, error))?;
    Ok(read)
}

fn ensure_handle(active: &AtomicBool, open: bool) -> Result<()> {
    if !active.load(Ordering::Acquire) {
        Err(Error::runtime("I2C package has been revoked"))
    } else if !open {
        Err(Error::runtime("I2C handle is closed"))
    } else {
        Ok(())
    }
}

fn positive_u32(value: i64, field: &str) -> Result<u32> {
    let value = u32::try_from(value)
        .map_err(|_| Error::runtime(format!("{field} must be a positive 32-bit integer")))?;
    if value == 0 {
        Err(Error::runtime(format!("{field} must be greater than zero")))
    } else {
        Ok(value)
    }
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
    check_length(length)
}

fn check_length(length: usize) -> Result<usize> {
    if length <= MAX_TRANSFER_BYTES {
        Ok(length)
    } else {
        Err(Error::runtime(format!(
            "transfer length must not exceed {MAX_TRANSFER_BYTES} bytes"
        )))
    }
}

fn bus_error(label: &str, error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("I2C bus ({label}) transaction failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;
    use embedded_hal::i2c::{ErrorType, Operation};
    use portable_atomic::AtomicBool;

    use super::*;

    struct TestProvider {
        busy: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct TestOpenError;

    impl core::fmt::Display for TestOpenError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("I2C resources are busy")
        }
    }

    impl core::error::Error for TestOpenError {}

    struct TestBus {
        busy: Arc<AtomicBool>,
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            self.busy.store(false, Ordering::Release);
        }
    }

    impl I2cProvider for TestProvider {
        type Bus = TestBus;
        type Error = TestOpenError;

        fn open_i2c(
            &self,
            request: I2cRequest<'_>,
        ) -> core::result::Result<Self::Bus, Self::Error> {
            if request.scl != "D1" || request.sda != "D2" || self.busy.swap(true, Ordering::AcqRel)
            {
                return Err(TestOpenError);
            }
            Ok(TestBus {
                busy: Arc::clone(&self.busy),
            })
        }
    }

    impl ErrorType for TestBus {
        type Error = Infallible;
    }

    impl I2c for TestBus {
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

    #[test]
    fn lua_opens_uses_and_drops_an_i2c_bus() {
        let provider = Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        });
        let package = I2cPackage::new(Arc::clone(&provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install I2C package");

        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local i2c = require('i2c')\n\
                 local bus <close> = i2c.open('D1', 'D2', 400000)\n\
                 local bytes = bus:read(0x3c, 2)\n\
                 return bus:is_open() and #bytes == 2",
            )
            .eval_async(),
        )
        .expect("run I2C application");

        assert!(result);
        assert!(!provider.busy.load(Ordering::Acquire));
    }

    #[test]
    fn lua_rejects_invalid_runtime_configuration_before_opening() {
        let provider = Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        });
        let package = I2cPackage::new(Arc::clone(&provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install I2C package");

        let rejected: bool = lua
            .load(
                "local i2c = require('i2c')\n\
                 local bus, err = i2c.open('D1', 'D1', 400000)\n\
                 return bus == nil and type(err) == 'string'",
            )
            .eval()
            .expect("run invalid I2C application");

        assert!(rejected);
        assert!(!provider.busy.load(Ordering::Acquire));
    }
}
