//! Lua package for runtime SPI handles over Board-exposed resources.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, string::ToString};
use barracuda_board_hal::{ExposedIo, SpiProvider, SpiRequest};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use embedded_hal::spi::{MODE_0, MODE_1, MODE_2, MODE_3, Mode};
use embedded_hal_async::spi::SpiBus;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

const MAX_TRANSFER_BYTES: usize = 64 * 1024;

/// Shares the unified exposed-I/O owner with the `spi` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct SpiPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> SpiPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + SpiProvider,
    {
        Self {
            io: Arc::clone(&context.hal.exposed_io),
        }
    }
}

impl<Io> Plugin for SpiPlugin<Io>
where
    Io: ExposedIo + SpiProvider,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
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
            .register(SpiPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct SpiPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> SpiPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for SpiPackage<Io>
where
    Io: SpiProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let io = Arc::clone(&self.io);
        let active = Arc::clone(&self.active);
        lua.register_lib("spi", move |package| {
            package.register_with(
                "open_bus",
                move |lua,
                      (sck, mosi, miso, frequency_hz, mode): (
                    String,
                    Option<String>,
                    Option<String>,
                    i64,
                    i64,
                )| {
                    if !active.load(Ordering::Acquire) {
                        return Some(Err(Error::runtime("SPI package has been revoked")));
                    }
                    let result = (|| {
                        validate_data_pins(mosi.as_deref(), miso.as_deref())?;
                        validate_distinct_pins(&sck, mosi.as_deref(), miso.as_deref())?;
                        let frequency_hz = positive_u32(frequency_hz, "SPI frequency")?;
                        let mode = parse_mode(mode)?;
                        let bus = io
                            .open_spi(SpiRequest {
                                sck: &sck,
                                mosi: mosi.as_deref(),
                                miso: miso.as_deref(),
                                frequency_hz,
                                mode,
                            })
                            .map_err(|error| Error::runtime(error.to_string()))?;
                        let label = format!("SCK `{sck}`");
                        lua.create_userdata(SpiHandle::new(label, bus, Arc::clone(&active)))
                    })();
                    Some(result)
                },
            )
        })
    }
}

impl<Io> LuaPackage for SpiPackage<Io>
where
    Io: SpiProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "spi"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct SpiHandle<Bus> {
    label: String,
    bus: Option<Bus>,
    active: Arc<AtomicBool>,
}

impl<Bus> SpiHandle<Bus> {
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

impl<Bus> UserData for SpiHandle<Bus>
where
    Bus: SpiBus + Send + 'static,
    <Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method("read", |handle, length: i64| async move {
            Some(spi_read(handle, length).await)
        });
        methods.add_async_method("write", |handle, bytes: Bytes| async move {
            Some(spi_write(handle, bytes).await)
        });
        methods.add_async_method(
            "transfer",
            |handle, (bytes, read_length): (Bytes, i64)| async move {
                Some(spi_transfer(handle, bytes, read_length).await)
            },
        );
        methods.add_async_method("transfer_in_place", |handle, bytes: Bytes| async move {
            Some(spi_transfer_in_place(handle, bytes).await)
        });
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

async fn spi_read<Bus>(handle: UserDataHandle<SpiHandle<Bus>>, length: i64) -> Result<Bytes>
where
    Bus: SpiBus + Send + 'static,
    <Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    let mut bytes = Bytes::zeroed(parse_length(length)?)?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("SPI handle is closed"))?
        .read(&mut bytes)
        .await
        .map_err(|error| bus_error(&label, error))?;
    Ok(bytes)
}

async fn spi_write<Bus>(handle: UserDataHandle<SpiHandle<Bus>>, bytes: Bytes) -> Result<()>
where
    Bus: SpiBus + Send + 'static,
    <Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    check_length(bytes.len())?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("SPI handle is closed"))?
        .write(&bytes)
        .await
        .map_err(|error| bus_error(&label, error))
}

async fn spi_transfer<Bus>(
    handle: UserDataHandle<SpiHandle<Bus>>,
    write: Bytes,
    read_length: i64,
) -> Result<Bytes>
where
    Bus: SpiBus + Send + 'static,
    <Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    check_length(write.len())?;
    let mut read = Bytes::zeroed(parse_length(read_length)?)?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("SPI handle is closed"))?
        .transfer(&mut read, &write)
        .await
        .map_err(|error| bus_error(&label, error))?;
    Ok(read)
}

async fn spi_transfer_in_place<Bus>(
    handle: UserDataHandle<SpiHandle<Bus>>,
    mut bytes: Bytes,
) -> Result<Bytes>
where
    Bus: SpiBus + Send + 'static,
    <Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
{
    check_length(bytes.len())?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.bus.is_some())?;
    let label = handle.label.clone();
    handle
        .bus
        .as_mut()
        .ok_or_else(|| Error::runtime("SPI handle is closed"))?
        .transfer_in_place(&mut bytes)
        .await
        .map_err(|error| bus_error(&label, error))?;
    Ok(bytes)
}

fn validate_data_pins(mosi: Option<&str>, miso: Option<&str>) -> Result<()> {
    if mosi.is_none() && miso.is_none() {
        Err(Error::runtime("SPI requires MOSI, MISO, or both"))
    } else {
        Ok(())
    }
}

fn validate_distinct_pins(sck: &str, mosi: Option<&str>, miso: Option<&str>) -> Result<()> {
    if mosi == Some(sck) || miso == Some(sck) || (mosi.is_some() && mosi == miso) {
        Err(Error::runtime("SPI signal roles must use different pins"))
    } else {
        Ok(())
    }
}

fn parse_mode(value: i64) -> Result<Mode> {
    match value {
        0 => Ok(MODE_0),
        1 => Ok(MODE_1),
        2 => Ok(MODE_2),
        3 => Ok(MODE_3),
        _ => Err(Error::runtime("SPI mode must be 0, 1, 2, or 3")),
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

fn ensure_handle(active: &AtomicBool, open: bool) -> Result<()> {
    if !active.load(Ordering::Acquire) {
        Err(Error::runtime("SPI package has been revoked"))
    } else if !open {
        Err(Error::runtime("SPI handle is closed"))
    } else {
        Ok(())
    }
}

fn bus_error(label: &str, error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("SPI bus ({label}) transaction failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;
    use embedded_hal::spi::ErrorType;
    use portable_atomic::AtomicBool;

    use super::*;

    struct TestProvider {
        busy: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct TestOpenError;

    impl core::fmt::Display for TestOpenError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("SPI resources are busy")
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

    impl SpiProvider for TestProvider {
        type Bus = TestBus;
        type Error = TestOpenError;

        fn open_spi(
            &self,
            request: SpiRequest<'_>,
        ) -> core::result::Result<Self::Bus, Self::Error> {
            if request.sck != "D1"
                || request.mosi != Some("D2")
                || request.miso != Some("D3")
                || self.busy.swap(true, Ordering::AcqRel)
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

    impl SpiBus for TestBus {
        async fn read(&mut self, words: &mut [u8]) -> core::result::Result<(), Self::Error> {
            words.fill(0x5a);
            Ok(())
        }

        async fn write(&mut self, _words: &[u8]) -> core::result::Result<(), Self::Error> {
            Ok(())
        }

        async fn transfer(
            &mut self,
            read: &mut [u8],
            _write: &[u8],
        ) -> core::result::Result<(), Self::Error> {
            read.fill(0xa5);
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
    fn lua_opens_uses_and_drops_an_spi_bus() {
        let provider = Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        });
        let package = SpiPackage::new(Arc::clone(&provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install SPI package");

        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local spi = require('spi')\n\
                 local bus <close> = spi.open_bus('D1', 'D2', 'D3', 10000000, 0)\n\
                 local bytes = bus:read(3)\n\
                 return bus:is_open() and #bytes == 3",
            )
            .eval_async(),
        )
        .expect("run SPI application");

        assert!(result);
        assert!(!provider.busy.load(Ordering::Acquire));
    }

    #[test]
    fn lua_rejects_duplicate_spi_roles_before_opening() {
        let provider = Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        });
        let package = SpiPackage::new(Arc::clone(&provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install SPI package");

        let rejected: bool = lua
            .load(
                "local spi = require('spi')\n\
                 local bus, err = spi.open_bus('D1', 'D1', nil, 10000000, 0)\n\
                 return bus == nil and type(err) == 'string'",
            )
            .eval()
            .expect("run invalid SPI application");

        assert!(rejected);
        assert!(!provider.busy.load(Ordering::Acquire));
    }
}
