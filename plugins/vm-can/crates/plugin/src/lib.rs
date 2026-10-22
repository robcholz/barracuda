//! Lua package for the Board's primary built-in CAN bus.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::can::{BusState, Can, CanPeripheral, Frame, Id, MAX_STANDARD_ID};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result, Table, Variadic,
};
use core::cell::RefCell;
use embassy_time::{Duration, with_timeout};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's primary CAN bus and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct CanPlugin<Device> {
    bus: Option<Device>,
}

impl<Device> CanPlugin<Device> {
    /// Takes the built-in CAN controller from the generated Board capability
    /// set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: CanPeripheral<Can = Device>,
    {
        Self {
            bus: config::peripherals::CAN_DEVICE
                .and_then(|instance| context.hal.peripherals.take_can(instance)),
        }
    }
}

impl<Device> Plugin for CanPlugin<Device>
where
    Device: Can + Send + 'static,
    Device::Error: core::fmt::Debug,
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
            .register(CanPackage::new(self.bus.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The controller while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The controller taken out of its slot for one call; it goes back when the
/// call ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("can package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime("built-in CAN bus is unavailable or busy")),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in CAN bus is unavailable or busy"))
    }
}

impl<Device> Drop for Lent<Device> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            critical_section::with(|section| {
                self.home.borrow(section).replace(Some(device));
            });
        }
    }
}

struct CanPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    bus: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CanPackage<Device> {
    fn new(bus: Option<Device>) -> Self {
        Self {
            present: bus.is_some(),
            bus: Arc::new(critical_section::Mutex::new(RefCell::new(bus))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn bus_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("CAN operation failed: {error:?}"))
}

/// A call's arguments: at least `required`, at most `N`; absent ones are nil.
fn arguments<const N: usize>(values: Variadic<LuaValue>, required: usize) -> Result<[LuaValue; N]> {
    let values: Vec<LuaValue> = values.into();
    if values.len() < required || values.len() > N {
        return Err(Error::runtime(alloc::format!(
            "expected {required} to {N} arguments"
        )));
    }
    let mut values = values.into_iter();
    Ok(core::array::from_fn(|_| {
        values.next().unwrap_or(LuaValue::Nil)
    }))
}

/// A frame identifier: extended when asked, or when it does not fit 11 bits.
fn identifier(id: i64, extended: bool) -> Result<Id> {
    let id = if extended || id > i64::from(MAX_STANDARD_ID) {
        u32::try_from(id).ok().and_then(Id::extended)
    } else {
        u16::try_from(id).ok().and_then(Id::standard)
    };
    id.ok_or_else(|| Error::runtime("a CAN identifier is 0 to 0x1FFFFFFF"))
}

fn state_name(state: BusState) -> &'static str {
    match state {
        BusState::Stopped => "stopped",
        BusState::ErrorActive => "error-active",
        BusState::ErrorPassive => "error-passive",
        BusState::BusOff => "bus-off",
    }
}

/// Fills `table` with `{id = ..., extended = ..., remote = ..., data = ...,
/// length = ...}`.
fn describe(table: &Table, frame: &Frame) -> Result<()> {
    table.set("id", i64::from(frame.id().raw()))?;
    table.set("extended", frame.id().is_extended())?;
    table.set("remote", frame.is_remote())?;
    table.set("data", Bytes::copy_from(frame.data())?)?;
    table.set("length", i64::from(frame.dlc()))
}

/// A frame, or nil and "timeout".
type ReceiveResult = (Option<Table>, Option<String>);

impl<Device> Package for CanPackage<Device>
where
    Device: Can + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let start = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let stop = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let bitrate = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let state = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let send = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let request = (Arc::clone(&self.bus), Arc::clone(&self.active));
        let receive = (Arc::clone(&self.bus), Arc::clone(&self.active));
        lua.register_lib("can", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register("start", move |bitrate: i64| {
                let (slot, active) = &start;
                Some((|| {
                    let bitrate = u32::try_from(bitrate)
                        .ok()
                        .filter(|bitrate| *bitrate > 0)
                        .ok_or_else(|| Error::runtime("a bit rate is a positive integer"))?;
                    let mut lent = Lent::take(slot, active)?;
                    let device = lent.device()?;
                    if !device.supports_bitrate(bitrate) {
                        return Err(Error::runtime(alloc::format!(
                            "the CAN controller cannot run at {bitrate} bit/s"
                        )));
                    }
                    device.start(bitrate).map_err(bus_error)?;
                    Ok(true)
                })())
            })?;
            package.register("stop", move |(): ()| {
                let (slot, active) = &stop;
                Some((|| {
                    Lent::take(slot, active)?
                        .device()?
                        .stop()
                        .map_err(bus_error)?;
                    Ok(true)
                })())
            })?;
            package.register("bitrate", move |(): ()| {
                let (slot, active) = &bitrate;
                Some(
                    Lent::take(slot, active)
                        .and_then(|mut lent| Ok(lent.device()?.bitrate().map(i64::from))),
                )
            })?;
            package.register("state", move |(): ()| {
                let (slot, active) = &state;
                Some(
                    Lent::take(slot, active)
                        .and_then(|mut lent| Ok(String::from(state_name(lent.device()?.state())))),
                )
            })?;
            for (name, extended) in [("send", false), ("send_extended", true)] {
                let (slot, active) = (Arc::clone(&send.0), Arc::clone(&send.1));
                package.register_async(name, move |(id, data): (i64, Bytes)| {
                    let (slot, active) = (Arc::clone(&slot), Arc::clone(&active));
                    let frame = identifier(id, extended).and_then(|id| {
                        Frame::new(id, &data)
                            .ok_or_else(|| Error::runtime("CAN data is 0 to 8 bytes"))
                    });
                    async move {
                        Some(
                            async {
                                let frame = frame?;
                                let mut lent = Lent::take(&slot, &active)?;
                                lent.device()?.transmit(&frame).await.map_err(bus_error)?;
                                Ok(true)
                            }
                            .await,
                        )
                    }
                })?;
            }
            for (name, extended) in [("request", false), ("request_extended", true)] {
                let (slot, active) = (Arc::clone(&request.0), Arc::clone(&request.1));
                package.register_async(name, move |(id, length): (i64, i64)| {
                    let (slot, active) = (Arc::clone(&slot), Arc::clone(&active));
                    let frame = identifier(id, extended).and_then(|id| {
                        u8::try_from(length)
                            .ok()
                            .and_then(|length| Frame::remote(id, length))
                            .ok_or_else(|| Error::runtime("a requested length is 0 through 8"))
                    });
                    async move {
                        Some(
                            async {
                                let frame = frame?;
                                let mut lent = Lent::take(&slot, &active)?;
                                lent.device()?.transmit(&frame).await.map_err(bus_error)?;
                                Ok(true)
                            }
                            .await,
                        )
                    }
                })?;
            }
            package.register_async_with("receive", move |lua, values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&receive.0), Arc::clone(&receive.1));
                let table = lua.create_table();
                let limit = arguments(values, 0).and_then(|[timeout_ms]| match timeout_ms {
                    LuaValue::Nil => Ok(None),
                    LuaValue::Integer(milliseconds) => u64::try_from(milliseconds)
                        .map(|milliseconds| Some(Duration::from_millis(milliseconds)))
                        .map_err(|_| Error::runtime("a timeout is milliseconds, 0 or more")),
                    _ => Err(Error::runtime("a timeout is milliseconds, 0 or more")),
                });
                async move {
                    Some(
                        async {
                            let table = table?;
                            let limit = limit?;
                            let mut lent = Lent::take(&slot, &active)?;
                            let device = lent.device()?;
                            let frame = match limit {
                                None => Some(device.receive().await),
                                Some(limit) => with_timeout(limit, device.receive()).await.ok(),
                            };
                            let Some(frame) = frame else {
                                return Ok((None, Some(String::from("timeout"))));
                            };
                            describe(&table, &frame.map_err(bus_error)?)?;
                            Ok((Some(table), None))
                        }
                        .await,
                    ) as Option<Result<ReceiveResult>>
                }
            })
        })
    }
}

impl<Device> LuaPackage for CanPackage<Device>
where
    Device: Can + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "can"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "CAN 总线收发",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;

    /// A controller that receives every frame it sends.
    #[derive(Default)]
    struct Loopback {
        bitrate: Option<u32>,
        queue: Vec<Frame>,
    }

    impl Can for Loopback {
        type Error = &'static str;

        fn supports_bitrate(&self, bitrate: u32) -> bool {
            bitrate == 500_000 || bitrate == 125_000
        }

        fn start(&mut self, bitrate: u32) -> core::result::Result<(), Self::Error> {
            self.bitrate = Some(bitrate);
            Ok(())
        }

        fn stop(&mut self) -> core::result::Result<(), Self::Error> {
            self.bitrate = None;
            Ok(())
        }

        fn bitrate(&self) -> Option<u32> {
            self.bitrate
        }

        fn state(&self) -> BusState {
            if self.bitrate.is_some() {
                BusState::ErrorActive
            } else {
                BusState::Stopped
            }
        }

        async fn transmit(&mut self, frame: &Frame) -> core::result::Result<(), Self::Error> {
            self.bitrate.ok_or("stopped")?;
            self.queue.push(*frame);
            Ok(())
        }

        async fn receive(&mut self) -> core::result::Result<Frame, Self::Error> {
            if self.queue.is_empty() {
                core::future::pending::<()>().await;
            }
            Ok(self.queue.remove(0))
        }
    }

    fn run(package: &CanPackage<Loopback>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_starts_sends_and_receives_frames() {
        let package = CanPackage::new(Some(Loopback::default()));
        assert!(run(
            &package,
            "local can = require('can')\n\
             assert(can.available() and can.state() == 'stopped' and can.bitrate() == nil)\n\
             local ok, err = can.start(83333)\n\
             assert(ok == nil and err:find('83333'))\n\
             assert(can.start(500000) and can.bitrate() == 500000)\n\
             assert(can.state() == 'error-active')\n\
             assert(can.send(0x123, '\\1\\2\\3'))\n\
             assert(can.send_extended(0x10, 'ab'))\n\
             assert(can.request(0x18ff0001, 4))\n\
             local a = can.receive(100)\n\
             assert(a.id == 0x123 and not a.extended and a.data == '\\1\\2\\3' and a.length == 3)\n\
             local b = can.receive(100)\n\
             assert(b.id == 0x10 and b.extended and b.data == 'ab')\n\
             local c = can.receive(100)\n\
             assert(c.extended and c.remote and c.length == 4 and c.data == '')\n\
             local none, why = can.receive(10)\n\
             assert(none == nil and why == 'timeout')\n\
             ok, err = can.send(0x123, '123456789')\n\
             assert(ok == nil and err:find('8 bytes'))\n\
             ok, err = can.send(0x20000000, '')\n\
             assert(ok == nil and err:find('identifier'))\n\
             return can.stop() and can.state() == 'stopped'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = CanPackage::new(Some(Loopback::default()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
