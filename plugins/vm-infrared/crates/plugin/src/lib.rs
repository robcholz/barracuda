//! Lua package for the Board's built-in infrared transceivers.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::infrared::{Carrier, Infrared, InfraredPeripheral, NecCommand, Received};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Variadic,
};
use embassy_time::{Duration, with_timeout};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most timings one raw send or receive carries.
const MAX_TIMINGS: usize = 512;

/// Owns the Board's infrared transceivers and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct InfraredPlugin<Device> {
    transceivers: Vec<(&'static str, Device)>,
}

impl<Device> InfraredPlugin<Device> {
    /// Takes the built-in transceivers from the generated Board capability
    /// set, the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: InfraredPeripheral<Infrared = Device>,
    {
        Self {
            transceivers: config::peripherals::INFRARED_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_infrared(*instance)
                        .map(|infrared| (*name, infrared))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for InfraredPlugin<Device>
where
    Device: Infrared + Send + 'static,
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
            .register(InfraredPackage::new(core::mem::take(
                &mut self.transceivers,
            )))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Lends the transceiver `name`, or the primary one, to one call; it goes
/// back when the call ends or is cancelled.
fn lend<Device>(
    transceivers: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
) -> Result<LentDevice<Device>> {
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("infrared package has been revoked"));
    }
    transceivers.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("infrared transceiver `{name}` is unavailable or busy"),
            None => String::from("built-in infrared transceiver is unavailable or busy"),
        })
    })
}

struct InfraredPackage<Device> {
    transceivers: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> InfraredPackage<Device> {
    fn new(transceivers: Vec<(&'static str, Device)>) -> Self {
        Self {
            transceivers: BoardDevices::new(transceivers),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn nec_command(address: i64, command: i64) -> Result<NecCommand> {
    let command =
        u8::try_from(command).map_err(|_| Error::runtime("an NEC command is 0 through 255"))?;
    match (u8::try_from(address), u16::try_from(address)) {
        (Ok(address), _) => Ok(NecCommand::new(address, command)),
        (_, Ok(address)) => Ok(NecCommand::extended(address, command)),
        _ => Err(Error::runtime("an NEC address is 0 through 65535")),
    }
}

/// Reads `{mark, space, mark, ...}` in microseconds.
fn timings(table: &Table) -> Result<Vec<u16>> {
    let length = table.len()?;
    if length > MAX_TIMINGS {
        return Err(Error::runtime("a pulse train holds at most 512 timings"));
    }
    let mut timings = Vec::with_capacity(length);
    for index in 1..=length {
        let timing: i64 = table.get(i64::try_from(index).unwrap_or(i64::MAX))?;
        timings.push(
            u16::try_from(timing)
                .map_err(|_| Error::runtime("a timing is 0 through 65535 microseconds"))?,
        );
    }
    Ok(timings)
}

fn carrier(frequency_hz: Option<i64>, duty_percent: Option<i64>) -> Result<Carrier> {
    let frequency_hz = match frequency_hz {
        None => Carrier::NEC.frequency_hz(),
        Some(frequency_hz) => u32::try_from(frequency_hz)
            .ok()
            .filter(|frequency_hz| *frequency_hz != 0)
            .ok_or_else(|| Error::runtime("a carrier is positive hertz"))?,
    };
    let duty_percent = match duty_percent {
        None => Carrier::NEC.duty_percent(),
        Some(duty_percent) => u8::try_from(duty_percent)
            .ok()
            .filter(|duty_percent| (1..=99).contains(duty_percent))
            .ok_or_else(|| Error::runtime("a carrier duty is 1 through 99 percent"))?,
    };
    Ok(Carrier::new(frequency_hz, duty_percent))
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

/// An integer argument, or `None` when it is nil.
fn optional_integer(value: &LuaValue) -> Result<Option<i64>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(value) => Ok(Some(*value)),
        _ => Err(Error::runtime("expected an integer argument")),
    }
}

/// A table argument.
fn table(value: LuaValue) -> Result<Table> {
    match value {
        LuaValue::Table(table) => Ok(table),
        _ => Err(Error::runtime("expected a table argument")),
    }
}

/// An integer argument that must be given.
fn integer(value: &LuaValue) -> Result<i64> {
    optional_integer(value)?.ok_or_else(|| Error::runtime("expected an integer argument"))
}

/// A transceiver name argument, or `None` for the primary one when it is nil.
fn device_name(value: LuaValue) -> Result<Option<String>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(name) => Ok(Some(name)),
        _ => Err(Error::runtime("expected an infrared transceiver name")),
    }
}

fn infrared_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("infrared operation failed: {error:?}"))
}

/// What `receive` returns: the frame kind and its values.
type ReceiveResult = (Option<String>, Option<LuaValue>, Option<LuaValue>);

impl<Device> Package for InfraredPackage<Device>
where
    Device: Infrared + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.transceivers.clone(), Arc::clone(&self.active));
        let list = self.transceivers.clone();
        let can_receive = (self.transceivers.clone(), Arc::clone(&self.active));
        let nec = (self.transceivers.clone(), Arc::clone(&self.active));
        let raw = (self.transceivers.clone(), Arc::clone(&self.active));
        let receive = (self.transceivers.clone(), Arc::clone(&self.active));
        lua.register_lib("infrared", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (transceivers, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && transceivers
                        .is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("can_receive", move |name: Variadic<String>| {
                let (transceivers, active) = &can_receive;
                Some(
                    lend(transceivers, active, name.first().map(String::as_str))
                        .map(|lent| lent.can_receive()),
                )
            })?;
            package.register_async("send_nec", move |values: Variadic<LuaValue>| {
                let (slot, active) = (nec.0.clone(), Arc::clone(&nec.1));
                let parsed = arguments(values, 2).and_then(|[address, command, name]| {
                    Ok((
                        nec_command(integer(&address)?, integer(&command)?)?,
                        device_name(name)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (command, name) = parsed?;
                            let mut lent = lend(&slot, &active, name.as_deref())?;
                            lent.send_nec(command).await.map_err(infrared_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("send_raw", move |values: Variadic<LuaValue>| {
                let (slot, active) = (raw.0.clone(), Arc::clone(&raw.1));
                let parsed = arguments(values, 1).and_then(|[list, frequency, duty, name]| {
                    Ok((
                        timings(&table(list)?)?,
                        carrier(optional_integer(&frequency)?, optional_integer(&duty)?)?,
                        device_name(name)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (timings, carrier, name) = parsed?;
                            let mut lent = lend(&slot, &active, name.as_deref())?;
                            lent.send_raw(carrier, &timings)
                                .await
                                .map_err(infrared_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async_with("receive", move |lua, values: Variadic<LuaValue>| {
                let (slot, active) = (receive.0.clone(), Arc::clone(&receive.1));
                // The table a raw frame comes back in is made before the wait,
                // while the Lua context is at hand.
                let table = lua.create_table();
                async move {
                    Some(
                        async {
                            let table = table?;
                            let [timeout_ms, name] = arguments(values, 0)?;
                            let name = device_name(name)?;
                            let timeout = match optional_integer(&timeout_ms)? {
                                None => None,
                                Some(milliseconds) => Some(Duration::from_millis(
                                    u64::try_from(milliseconds).map_err(|_| {
                                        Error::runtime("a timeout is milliseconds, 0 or more")
                                    })?,
                                )),
                            };
                            let mut lent = lend(&slot, &active, name.as_deref())?;
                            let device = &mut *lent;
                            if !device.can_receive() {
                                return Err(Error::runtime("the Board has no infrared receiver"));
                            }
                            let mut buffer = alloc::vec![0u16; MAX_TIMINGS];
                            let received = match timeout {
                                None => Some(device.receive(&mut buffer).await),
                                Some(timeout) => with_timeout(timeout, device.receive(&mut buffer))
                                    .await
                                    .ok(),
                            };
                            let Some(received) = received else {
                                return Ok((
                                    None,
                                    Some(LuaValue::String(String::from("timeout"))),
                                    None,
                                ));
                            };
                            Ok(match received.map_err(infrared_error)? {
                                Received::Nec(command) => (
                                    Some(String::from("nec")),
                                    Some(LuaValue::Integer(i64::from(command.address()))),
                                    Some(LuaValue::Integer(i64::from(command.command()))),
                                ),
                                Received::NecRepeat => (Some(String::from("repeat")), None, None),
                                Received::Raw { len } => {
                                    for (index, timing) in buffer.iter().take(len).enumerate() {
                                        table.set(
                                            i64::try_from(index).unwrap_or(i64::MAX) + 1,
                                            i64::from(*timing),
                                        )?;
                                    }
                                    (
                                        Some(String::from("raw")),
                                        Some(LuaValue::Table(table)),
                                        None,
                                    )
                                }
                            })
                        }
                        .await,
                    ) as Option<Result<ReceiveResult>>
                }
            })
        })
    }
}

impl<Device> LuaPackage for InfraredPackage<Device>
where
    Device: Infrared + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "infrared"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.transceivers.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "红外遥控收发",
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
    use core::cell::RefCell;
    use core::convert::Infallible;
    use std::vec::Vec;

    /// What a test device records, shared with the test.
    type Log<T> = Arc<critical_section::Mutex<RefCell<Vec<T>>>>;

    /// Records what it sends and replays a fixed reception.
    struct TestInfrared {
        sent: Log<(u32, Vec<u16>)>,
        receptions: Vec<Received>,
    }

    impl Default for TestInfrared {
        fn default() -> Self {
            Self {
                sent: Arc::new(critical_section::Mutex::new(RefCell::new(Vec::new()))),
                receptions: Vec::new(),
            }
        }
    }

    impl Infrared for TestInfrared {
        type Error = Infallible;

        fn can_receive(&self) -> bool {
            true
        }

        async fn send_nec(&mut self, command: NecCommand) -> core::result::Result<(), Infallible> {
            let timings = std::vec![command.address(), u16::from(command.command())];
            critical_section::with(|section| {
                self.sent.borrow(section).borrow_mut().push((0, timings));
            });
            Ok(())
        }

        async fn send_raw(
            &mut self,
            carrier: Carrier,
            timings_us: &[u16],
        ) -> core::result::Result<(), Infallible> {
            critical_section::with(|section| {
                self.sent
                    .borrow(section)
                    .borrow_mut()
                    .push((carrier.frequency_hz(), timings_us.to_vec()));
            });
            Ok(())
        }

        async fn receive(
            &mut self,
            timings_us: &mut [u16],
        ) -> core::result::Result<Received, Infallible> {
            let received = self.receptions.remove(0);
            if let Received::Raw { len } = received {
                for (index, slot) in timings_us.iter_mut().take(len).enumerate() {
                    *slot = u16::try_from(index + 1).unwrap_or(0) * 100;
                }
            }
            Ok(received)
        }
    }

    fn run(package: &InfraredPackage<TestInfrared>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_sends_nec_and_raw_and_receives_frames() {
        let infrared = TestInfrared {
            receptions: std::vec![
                Received::Nec(NecCommand::new(0x10, 0x20)),
                Received::NecRepeat,
                Received::Raw { len: 3 },
            ],
            ..TestInfrared::default()
        };
        let sent = Arc::clone(&infrared.sent);
        let package = InfraredPackage::new(std::vec![("main", infrared)]);
        assert!(run(
            &package,
            "local ir = require('infrared')\n\
             assert(ir.available() and ir.can_receive())\n\
             assert(ir.send_nec(0x1234, 7))\n\
             assert(ir.send_raw({9000, 4500, 560}, 40000))\n\
             local kind, address, command = ir.receive()\n\
             assert(kind == 'nec' and address == 16 and command == 32)\n\
             assert(ir.receive(1000) == 'repeat')\n\
             local raw, timings = ir.receive()\n\
             assert(raw == 'raw' and #timings == 3 and timings[3] == 300)\n\
             local ok, err = ir.send_nec(1, 300)\n\
             return ok == nil and err:find('command') ~= nil",
        ));
        let sent = critical_section::with(|section| sent.borrow(section).borrow().clone());
        assert_eq!(
            sent,
            [
                (0, std::vec![0x1234, 7]),
                (40_000, std::vec![9_000, 4_500, 560])
            ]
        );
    }

    #[test]
    fn lua_names_a_transceiver_and_defaults_to_the_primary() {
        let (main, side) = (TestInfrared::default(), TestInfrared::default());
        let (main_sent, side_sent) = (Arc::clone(&main.sent), Arc::clone(&side.sent));
        let package = InfraredPackage::new(std::vec![("main", main), ("side", side)]);
        assert!(run(
            &package,
            "local ir = require('infrared')\n\
             local names = ir.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(ir.can_receive('side'))\n\
             assert(ir.send_nec(1, 2))\n\
             assert(ir.send_nec(3, 4, 'side'))\n\
             assert(ir.send_raw({100}, nil, nil, 'side'))\n\
             local ok, err = ir.send_nec(5, 6, 'missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
        let sent = |log: &Log<(u32, Vec<u16>)>| {
            critical_section::with(|section| log.borrow(section).borrow().clone())
        };
        assert_eq!(sent(&main_sent), [(0, std::vec![1, 2])]);
        assert_eq!(
            sent(&side_sent),
            [(0, std::vec![3, 4]), (38_000, std::vec![100])]
        );
    }

    #[test]
    fn a_board_without_infrared_reports_none() {
        let package = InfraredPackage::<TestInfrared>::new(Vec::new());
        assert!(run(
            &package,
            "local ir = require('infrared')\n\
             local ok, err = ir.send_nec(1, 2)\n\
             return not ir.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = InfraredPackage::new(std::vec![("main", TestInfrared::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
