//! Lua package for the Board's built-in CAN buses.

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
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Trailing, Variadic,
};
use embassy_time::{Duration, with_timeout};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's CAN buses and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct CanPlugin<Device> {
    buses: Vec<(&'static str, Device)>,
}

impl<Device> CanPlugin<Device> {
    /// Takes the built-in CAN buses from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: CanPeripheral<Can = Device>,
    {
        Self {
            buses: config::peripherals::CAN_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_can(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
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
            .register(CanPackage::new(core::mem::take(&mut self.buses)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The CAN bus lent to one call; it goes back when the call ends or is
/// cancelled.
struct Lent<Device>(LentDevice<Device>);

impl<Device> Lent<Device> {
    /// Lends the CAN bus `name`, or the primary one.
    fn take(
        devices: &BoardDevices<Device>,
        active: &AtomicBool,
        name: Option<&str>,
    ) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("can package has been revoked"));
        }
        devices.lend(name).map(Self).ok_or_else(|| {
            Error::runtime(match name {
                Some(name) => alloc::format!("CAN bus `{name}` is unavailable or busy"),
                None => String::from("built-in CAN bus is unavailable or busy"),
            })
        })
    }

    fn device(&mut self) -> &mut Device {
        &mut self.0
    }
}

struct CanPackage<Device> {
    buses: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CanPackage<Device> {
    fn new(buses: Vec<(&'static str, Device)>) -> Self {
        Self {
            buses: BoardDevices::new(buses),
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
        let available = (self.buses.clone(), Arc::clone(&self.active));
        let list = self.buses.clone();
        let start = (self.buses.clone(), Arc::clone(&self.active));
        let stop = (self.buses.clone(), Arc::clone(&self.active));
        let bitrate = (self.buses.clone(), Arc::clone(&self.active));
        let state = (self.buses.clone(), Arc::clone(&self.active));
        let send = (self.buses.clone(), Arc::clone(&self.active));
        let request = (self.buses.clone(), Arc::clone(&self.active));
        let receive = (self.buses.clone(), Arc::clone(&self.active));
        lua.register_lib("can", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (buses, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && buses.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register(
                "start",
                move |Trailing((bitrate,), name): Trailing<(i64,), String>| {
                    let (slot, active) = &start;
                    Some((|| {
                        let bitrate = u32::try_from(bitrate)
                            .ok()
                            .filter(|bitrate| *bitrate > 0)
                            .ok_or_else(|| Error::runtime("a bit rate is a positive integer"))?;
                        let mut lent = Lent::take(slot, active, name.as_deref())?;
                        let device = lent.device();
                        if !device.supports_bitrate(bitrate) {
                            return Err(Error::runtime(alloc::format!(
                                "the CAN controller cannot run at {bitrate} bit/s"
                            )));
                        }
                        device.start(bitrate).map_err(bus_error)?;
                        Ok(true)
                    })())
                },
            )?;
            package.register("stop", move |name: Variadic<String>| {
                let (slot, active) = &stop;
                Some((|| {
                    Lent::take(slot, active, name.first().map(String::as_str))?
                        .device()
                        .stop()
                        .map_err(bus_error)?;
                    Ok(true)
                })())
            })?;
            package.register("bitrate", move |name: Variadic<String>| {
                let (slot, active) = &bitrate;
                Some(
                    Lent::take(slot, active, name.first().map(String::as_str))
                        .map(|mut lent| lent.device().bitrate().map(i64::from)),
                )
            })?;
            package.register("state", move |name: Variadic<String>| {
                let (slot, active) = &state;
                Some(
                    Lent::take(slot, active, name.first().map(String::as_str))
                        .map(|mut lent| String::from(state_name(lent.device().state()))),
                )
            })?;
            for (name, extended) in [("send", false), ("send_extended", true)] {
                let (slot, active) = (send.0.clone(), Arc::clone(&send.1));
                package.register_async(
                    name,
                    move |Trailing((id, data), name): Trailing<(i64, Bytes), String>| {
                        let (slot, active) = (slot.clone(), Arc::clone(&active));
                        let frame = identifier(id, extended).and_then(|id| {
                            Frame::new(id, &data)
                                .ok_or_else(|| Error::runtime("CAN data is 0 to 8 bytes"))
                        });
                        async move {
                            Some(
                                async {
                                    let frame = frame?;
                                    let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                    lent.device().transmit(&frame).await.map_err(bus_error)?;
                                    Ok(true)
                                }
                                .await,
                            )
                        }
                    },
                )?;
            }
            for (name, extended) in [("request", false), ("request_extended", true)] {
                let (slot, active) = (request.0.clone(), Arc::clone(&request.1));
                package.register_async(
                    name,
                    move |Trailing((id, length), name): Trailing<(i64, i64), String>| {
                        let (slot, active) = (slot.clone(), Arc::clone(&active));
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
                                    let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                    lent.device().transmit(&frame).await.map_err(bus_error)?;
                                    Ok(true)
                                }
                                .await,
                            )
                        }
                    },
                )?;
            }
            package.register_async_with("receive", move |lua, values: Variadic<LuaValue>| {
                let (slot, active) = (receive.0.clone(), Arc::clone(&receive.1));
                let table = lua.create_table();
                let limit = arguments(values, 0).and_then(|[timeout_ms, name]| {
                    let limit = match timeout_ms {
                        LuaValue::Nil => None,
                        LuaValue::Integer(milliseconds) => {
                            Some(Duration::from_millis(u64::try_from(milliseconds).map_err(
                                |_| Error::runtime("a timeout is milliseconds, 0 or more"),
                            )?))
                        }
                        _ => return Err(Error::runtime("a timeout is milliseconds, 0 or more")),
                    };
                    let name = match name {
                        LuaValue::Nil => None,
                        LuaValue::String(name) => Some(name),
                        _ => return Err(Error::runtime("expected a CAN bus name")),
                    };
                    Ok((limit, name))
                });
                async move {
                    Some(
                        async {
                            let table = table?;
                            let (limit, name) = limit?;
                            let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                            let device = lent.device();
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
        (!self.buses.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
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
        let package = CanPackage::new(std::vec![("main", Loopback::default())]);
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
    fn lua_names_a_bus_and_defaults_to_the_primary() {
        let package = CanPackage::new(std::vec![
            ("main", Loopback::default()),
            ("side", Loopback::default()),
        ]);
        assert!(run(
            &package,
            "local can = require('can')\n\
             local names = can.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(can.start(125000, 'side'))\n\
             assert(can.bitrate() == nil and can.bitrate('side') == 125000)\n\
             assert(can.send(0x7, 'x', 'side'))\n\
             local frame = can.receive(100, 'side')\n\
             assert(frame.id == 0x7 and frame.data == 'x')\n\
             local ok, err = can.send(0x7, 'x')\n\
             assert(ok == nil and err:find('stopped'))\n\
             ok, err = can.stop('missing')\n\
             return ok == nil and err:find('missing') ~= nil and can.state('side') == 'error-active'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = CanPackage::new(std::vec![("main", Loopback::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
