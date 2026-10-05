//! Lua package for UART streams on Board-exposed pins.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, string::ToString, vec, vec::Vec};
use barracuda_board_hal::{
    ExposedIo, UartConfig, UartDataBits, UartParity, UartProvider, UartRequest, UartStopBits,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use embedded_io_async::{Read, Write};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

const MAX_TRANSFER_BYTES: usize = 64 * 1024;

/// Shares the unified exposed-I/O owner with the `uart` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct UartPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> UartPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + UartProvider,
    {
        Self {
            io: Arc::clone(&context.hal.exposed_io),
        }
    }
}

impl<Io> Plugin for UartPlugin<Io>
where
    Io: ExposedIo + UartProvider,
    Io::Error: core::fmt::Display,
    <Io::Port as embedded_io::ErrorType>::Error: core::fmt::Debug,
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
            .register(UartPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct UartPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> UartPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for UartPackage<Io>
where
    Io: UartProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Port as embedded_io::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = Arc::clone(&self.io);
        let available_active = Arc::clone(&self.active);
        let io = Arc::clone(&self.io);
        let active = Arc::clone(&self.active);
        lua.register_lib("uart", move |package| {
            package.register(
                "available",
                move |(tx, rx): (Option<String>, Option<String>)| {
                    Some(Ok(available_active.load(Ordering::Acquire)
                        && available.uart_available(tx.as_deref(), rx.as_deref())))
                },
            )?;
            package.register_with(
                "open",
                move |lua, args: (Option<String>, Option<String>, i64, i64, String, i64)| {
                    Some((|| {
                        ensure_active(&active)?;
                        let (tx, rx, baud, data_bits, parity, stop_bits) = args;
                        validate_pins(tx.as_deref(), rx.as_deref())?;
                        let config = UartConfig {
                            baud: positive_u32(baud)?,
                            data_bits: parse_data_bits(data_bits)?,
                            parity: parse_parity(&parity)?,
                            stop_bits: parse_stop_bits(stop_bits)?,
                        };
                        let port = io
                            .open_uart(UartRequest {
                                tx: tx.as_deref(),
                                rx: rx.as_deref(),
                                config,
                            })
                            .map_err(|error| Error::runtime(error.to_string()))?;
                        lua.create_userdata(UartHandle::new(port, Arc::clone(&active)))
                    })())
                },
            )
        })
    }
}

impl<Io> LuaPackage for UartPackage<Io>
where
    Io: UartProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Port as embedded_io::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "uart"
    }
    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct UartHandle<Port> {
    port: Option<Port>,
    active: Arc<AtomicBool>,
}

impl<Port> UartHandle<Port> {
    fn new(port: Port, active: Arc<AtomicBool>) -> Self {
        Self {
            port: Some(port),
            active,
        }
    }
    fn close(&mut self) {
        self.port.take();
    }
}

impl<Port> UserData for UartHandle<Port>
where
    Port: Read + Write + Send + 'static,
    Port::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method("read", |handle, length: i64| async move {
            Some(uart_read(handle, length).await)
        });
        methods.add_async_method("write", |handle, bytes: Vec<u8>| async move {
            Some(uart_write(handle, bytes).await)
        });
        methods.add_async_method("flush", |handle, (): ()| async move {
            Some(uart_flush(handle).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.port.is_some()
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

async fn uart_read<Port>(handle: UserDataHandle<UartHandle<Port>>, length: i64) -> Result<Vec<u8>>
where
    Port: Read + Write + Send + 'static,
    Port::Error: core::fmt::Debug,
{
    let mut bytes = vec![0; parse_length(length)?];
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.port.is_some())?;
    let read = handle
        .port
        .as_mut()
        .ok_or_else(closed)?
        .read(&mut bytes)
        .await
        .map_err(io_error)?;
    bytes.truncate(read);
    Ok(bytes)
}

async fn uart_write<Port>(handle: UserDataHandle<UartHandle<Port>>, bytes: Vec<u8>) -> Result<()>
where
    Port: Read + Write + Send + 'static,
    Port::Error: core::fmt::Debug,
{
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("UART write exceeds 65536 bytes"));
    }
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.port.is_some())?;
    handle
        .port
        .as_mut()
        .ok_or_else(closed)?
        .write_all(&bytes)
        .await
        .map_err(io_error)
}

async fn uart_flush<Port>(handle: UserDataHandle<UartHandle<Port>>) -> Result<()>
where
    Port: Read + Write + Send + 'static,
    Port::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.port.is_some())?;
    handle
        .port
        .as_mut()
        .ok_or_else(closed)?
        .flush()
        .await
        .map_err(io_error)
}

fn validate_pins(tx: Option<&str>, rx: Option<&str>) -> Result<()> {
    if tx.is_none() && rx.is_none() {
        Err(Error::runtime("UART requires TX or RX"))
    } else if tx.is_some() && tx == rx {
        Err(Error::runtime("UART TX and RX must use different pins"))
    } else {
        Ok(())
    }
}
fn positive_u32(value: i64) -> Result<u32> {
    let value = u32::try_from(value)
        .map_err(|_| Error::runtime("UART baud must be a positive 32-bit integer"))?;
    if value == 0 {
        Err(Error::runtime("UART baud must be greater than zero"))
    } else {
        Ok(value)
    }
}
fn parse_data_bits(value: i64) -> Result<UartDataBits> {
    match value {
        7 => Ok(UartDataBits::Seven),
        8 => Ok(UartDataBits::Eight),
        9 => Ok(UartDataBits::Nine),
        _ => Err(Error::runtime("UART data bits must be 7, 8, or 9")),
    }
}
fn parse_parity(value: &str) -> Result<UartParity> {
    match value {
        "none" => Ok(UartParity::None),
        "even" => Ok(UartParity::Even),
        "odd" => Ok(UartParity::Odd),
        _ => Err(Error::runtime(
            "UART parity must be 'none', 'even', or 'odd'",
        )),
    }
}
fn parse_stop_bits(value: i64) -> Result<UartStopBits> {
    match value {
        1 => Ok(UartStopBits::One),
        2 => Ok(UartStopBits::Two),
        _ => Err(Error::runtime("UART stop bits must be 1 or 2")),
    }
}
fn parse_length(value: i64) -> Result<usize> {
    let length = usize::try_from(value)
        .map_err(|_| Error::runtime("UART read length must be non-negative"))?;
    if length <= MAX_TRANSFER_BYTES {
        Ok(length)
    } else {
        Err(Error::runtime("UART read length exceeds 65536 bytes"))
    }
}
fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("UART package has been revoked"))
    }
}
fn ensure_handle(active: &AtomicBool, open: bool) -> Result<()> {
    ensure_active(active)?;
    if open { Ok(()) } else { Err(closed()) }
}
fn closed() -> Error {
    Error::runtime("UART handle is closed")
}
fn io_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("UART I/O failed: {error:?}"))
}

impl<Port> Drop for UartHandle<Port> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    extern crate std;
    use super::*;
    use core::convert::Infallible;

    struct TestPort;
    impl embedded_io::ErrorType for TestPort {
        type Error = Infallible;
    }
    impl Read for TestPort {
        async fn read(&mut self, buffer: &mut [u8]) -> core::result::Result<usize, Self::Error> {
            let bytes = b"OK";
            let len = bytes.len().min(buffer.len());
            buffer[..len].copy_from_slice(&bytes[..len]);
            Ok(len)
        }
    }
    impl Write for TestPort {
        async fn write(&mut self, buffer: &[u8]) -> core::result::Result<usize, Self::Error> {
            Ok(buffer.len())
        }
        async fn flush(&mut self) -> core::result::Result<(), Self::Error> {
            Ok(())
        }
    }
    #[derive(Debug)]
    struct OpenError;
    impl core::fmt::Display for OpenError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("unavailable")
        }
    }
    impl core::error::Error for OpenError {}
    struct Provider;
    impl UartProvider for Provider {
        type Port = TestPort;
        type Error = OpenError;
        fn uart_available(&self, tx: Option<&str>, rx: Option<&str>) -> bool {
            tx == Some("D1") && rx == Some("D2")
        }
        fn open_uart(
            &self,
            request: UartRequest<'_>,
        ) -> core::result::Result<Self::Port, Self::Error> {
            if self.uart_available(request.tx, request.rx) && request.config.baud == 115_200 {
                Ok(TestPort)
            } else {
                Err(OpenError)
            }
        }
    }

    #[test]
    fn lua_uses_an_embedded_io_uart_stream() {
        let package = UartPackage::new(Arc::new(Provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install UART package");
        let result: bool = futures_lite::future::block_on(lua.load("local uart = require('uart')\nlocal port <close> = uart.open('D1', 'D2', 115200, 8, 'none', 1)\nport:write('AT')\nlocal reply = port:read(16)\nreturn reply == 'OK' and port:is_open()").eval_async()).expect("use UART");
        assert!(result);
    }
}
