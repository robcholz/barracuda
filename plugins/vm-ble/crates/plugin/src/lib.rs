//! Lua package for the target's platform-independent BLE adapter.

#![no_std]

extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use barracuda_board_hal::{
    BleAdapter, BleAddress, BleAddressKind, BleAdvertisement, BleProvider, BleScanRequest,
    ExposedIo,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::sync::atomic::{AtomicBool, Ordering};

const MAX_SCAN_TIMEOUT_MILLIS: u32 = 60_000;

/// Shares the unified runtime owner with the `ble` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct BlePlugin<Io> {
    io: Arc<Io>,
}

impl<Io> BlePlugin<Io> {
    /// Acquires a shared handle to the Board HAL's runtime owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + BleProvider,
    {
        Self {
            io: Arc::clone(&context.hal.io),
        }
    }
}

impl<Io> Plugin for BlePlugin<Io>
where
    Io: ExposedIo + BleProvider,
    Io::Error: core::fmt::Display,
    <Io::Adapter as BleAdapter>::Error: core::fmt::Debug,
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
            .register(BlePackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct BlePackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> BlePackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for BlePackage<Io>
where
    Io: BleProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Adapter as BleAdapter>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = Arc::clone(&self.io);
        let available_active = Arc::clone(&self.active);
        let open = Arc::clone(&self.io);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("ble", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(
                    available_active.load(Ordering::Acquire) && available.ble_available()
                ))
            })?;
            package.register_with("open", move |lua, (): ()| {
                Some((|| {
                    ensure_active(&open_active)?;
                    let adapter = open
                        .take_ble()
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    lua.create_userdata(BleHandle::new(adapter, Arc::clone(&open_active)))
                })())
            })
        })
    }
}

impl<Io> LuaPackage for BlePackage<Io>
where
    Io: BleProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Adapter as BleAdapter>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "ble"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct BleHandle<Adapter> {
    adapter: Option<Adapter>,
    active: Arc<AtomicBool>,
}

impl<Adapter> BleHandle<Adapter> {
    fn new(adapter: Adapter, active: Arc<AtomicBool>) -> Self {
        Self {
            adapter: Some(adapter),
            active,
        }
    }

    fn close(&mut self) {
        self.adapter.take();
    }
}

impl<Adapter> UserData for BleHandle<Adapter>
where
    Adapter: BleAdapter,
    Adapter::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method(
            "scan",
            |handle, (active, timeout_millis): (bool, i64)| async move {
                Some(scan(handle, active, timeout_millis).await)
            },
        );
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.adapter.is_some()
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

type ScanResult = (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<bool>,
    Option<Vec<u8>>,
);

async fn scan<Adapter>(
    handle: UserDataHandle<BleHandle<Adapter>>,
    active: bool,
    timeout_millis: i64,
) -> Result<ScanResult>
where
    Adapter: BleAdapter,
    Adapter::Error: core::fmt::Debug,
{
    let timeout_millis = parse_timeout(timeout_millis)?;
    let mut handle = handle.borrow_mut()?;
    ensure_handle(&handle.active, handle.adapter.is_some())?;
    let advertisement = handle
        .adapter
        .as_mut()
        .ok_or_else(closed)?
        .scan(BleScanRequest {
            active,
            timeout_millis,
        })
        .await
        .map_err(|error| Error::runtime(format!("BLE scan failed: {error:?}")))?;
    Ok(match advertisement {
        Some(advertisement) => advertisement_result(advertisement),
        None => (None, None, None, None, None),
    })
}

fn advertisement_result(advertisement: BleAdvertisement) -> ScanResult {
    (
        Some(format_address(advertisement.address)),
        Some(match advertisement.address.kind {
            BleAddressKind::Public => String::from("public"),
            BleAddressKind::Random => String::from("random"),
        }),
        Some(i64::from(advertisement.rssi_dbm)),
        Some(advertisement.connectable),
        Some(advertisement.payload().to_vec()),
    )
}

fn format_address(address: BleAddress) -> String {
    let [a, b, c, d, e, f] = address.bytes;
    format!("{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{f:02X}")
}

fn parse_timeout(value: i64) -> Result<u32> {
    let timeout = u32::try_from(value)
        .map_err(|_| Error::runtime("BLE scan timeout must be a positive 32-bit integer"))?;
    if timeout == 0 || timeout > MAX_SCAN_TIMEOUT_MILLIS {
        Err(Error::runtime(format!(
            "BLE scan timeout must be between 1 and {MAX_SCAN_TIMEOUT_MILLIS} milliseconds"
        )))
    } else {
        Ok(timeout)
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("BLE package has been revoked"))
    }
}

fn ensure_handle(active: &AtomicBool, open: bool) -> Result<()> {
    ensure_active(active)?;
    if open { Ok(()) } else { Err(closed()) }
}

fn closed() -> Error {
    Error::runtime("BLE handle is closed")
}

impl<Adapter> Drop for BleHandle<Adapter> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;

    use super::*;

    struct TestAdapter;

    impl BleAdapter for TestAdapter {
        type Error = Infallible;

        async fn scan(
            &mut self,
            request: BleScanRequest,
        ) -> core::result::Result<Option<BleAdvertisement>, Self::Error> {
            assert_eq!(request.timeout_millis, 250);
            if !request.active {
                return Ok(None);
            }
            Ok(Some(
                BleAdvertisement::new(
                    BleAddress {
                        bytes: [0x01, 0x23, 0x45, 0x67, 0x89, 0xab],
                        kind: BleAddressKind::Random,
                    },
                    -42,
                    true,
                    b"\x02\x01\x06",
                )
                .expect("bounded advertisement"),
            ))
        }
    }

    #[derive(Debug)]
    struct OpenError;

    impl core::fmt::Display for OpenError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("BLE unavailable")
        }
    }

    impl core::error::Error for OpenError {}

    struct TestProvider {
        adapter: critical_section::Mutex<core::cell::RefCell<Option<TestAdapter>>>,
    }

    impl TestProvider {
        fn new() -> Self {
            Self {
                adapter: critical_section::Mutex::new(core::cell::RefCell::new(Some(TestAdapter))),
            }
        }
    }

    impl BleProvider for TestProvider {
        type Adapter = TestAdapter;
        type Error = OpenError;

        fn ble_available(&self) -> bool {
            critical_section::with(|section| self.adapter.borrow(section).borrow().is_some())
        }

        fn take_ble(&self) -> core::result::Result<Self::Adapter, Self::Error> {
            critical_section::with(|section| {
                self.adapter
                    .borrow(section)
                    .borrow_mut()
                    .take()
                    .ok_or(OpenError)
            })
        }
    }

    #[test]
    fn lua_scans_through_the_portable_ble_adapter() {
        let package = BlePackage::new(Arc::new(TestProvider::new()));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install BLE package");
        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local ble = require('ble')\n\
                 local available_before = ble.available()\n\
                 local adapter <close> = ble.open()\n\
                 local unavailable_after = not ble.available()\n\
                 local address, kind, rssi, connectable, payload = adapter:scan(true, 250)\n\
                 return available_before and unavailable_after and address == '01:23:45:67:89:AB' and kind == 'random' and rssi == -42 and connectable and payload == '\\2\\1\\6'",
            )
            .eval_async(),
        )
        .expect("scan with BLE");
        assert!(result);
    }

    #[test]
    fn lua_rejects_invalid_timeout_without_consuming_the_adapter() {
        let package = BlePackage::new(Arc::new(TestProvider::new()));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install BLE package");
        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local ble = require('ble')\n\
                 local adapter <close> = ble.open()\n\
                 local value, error = adapter:scan(false, 0)\n\
                 return value == nil and string.find(error, 'between 1 and 60000') ~= nil and adapter:is_open()",
            )
            .eval_async(),
        )
        .expect("reject invalid timeout");
        assert!(result);
    }

    #[test]
    fn scan_timeout_returns_five_nil_values() {
        let package = BlePackage::new(Arc::new(TestProvider::new()));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install BLE package");
        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local adapter <close> = require('ble').open()\n\
                 local address, kind, rssi, connectable, payload = adapter:scan(false, 250)\n\
                 return address == nil and kind == nil and rssi == nil and connectable == nil and payload == nil",
            )
            .eval_async(),
        )
        .expect("finish scan without an advertisement");
        assert!(result);
    }

    #[test]
    fn revocation_invalidates_an_existing_adapter() {
        let package = BlePackage::new(Arc::new(TestProvider::new()));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install BLE package");
        lua.load("ble = require('ble'); adapter = ble.open()")
            .exec()
            .expect("open BLE adapter");

        package.revoke();

        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local value, error = adapter:scan(true, 250)\n\
                 return not ble.available() and not adapter:is_open() and value == nil and string.find(error, 'revoked') ~= nil",
            )
            .eval_async(),
        )
        .expect("reject access after revocation");
        assert!(result);
    }
}
