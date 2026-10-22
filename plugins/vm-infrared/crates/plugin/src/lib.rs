//! Lua package for the Board's primary built-in infrared transceiver.

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
    Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result, Table, Variadic,
};
use core::cell::RefCell;
use embassy_time::{Duration, with_timeout};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most timings one raw send or receive carries.
const MAX_TIMINGS: usize = 512;

/// Owns the Board's primary infrared transceiver and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct InfraredPlugin<Device> {
    infrared: Option<Device>,
}

impl<Device> InfraredPlugin<Device> {
    /// Takes the built-in transceiver from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: InfraredPeripheral<Infrared = Device>,
    {
        Self {
            infrared: config::peripherals::INFRARED_DEVICE
                .and_then(|instance| context.hal.peripherals.take_infrared(instance)),
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
            .register(InfraredPackage::new(self.infrared.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The transceiver while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The transceiver taken out of its slot for one call; it goes back when the
/// call ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("infrared package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime(
                "built-in infrared transceiver is unavailable or busy",
            )),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in infrared transceiver is unavailable or busy"))
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

struct InfraredPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    infrared: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> InfraredPackage<Device> {
    fn new(infrared: Option<Device>) -> Self {
        Self {
            present: infrared.is_some(),
            infrared: Arc::new(critical_section::Mutex::new(RefCell::new(infrared))),
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
        let available = (Arc::clone(&self.infrared), Arc::clone(&self.active));
        let can_receive = (Arc::clone(&self.infrared), Arc::clone(&self.active));
        let nec = (Arc::clone(&self.infrared), Arc::clone(&self.active));
        let raw = (Arc::clone(&self.infrared), Arc::clone(&self.active));
        let receive = (Arc::clone(&self.infrared), Arc::clone(&self.active));
        lua.register_lib("infrared", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register("can_receive", move |(): ()| {
                let (slot, active) = &can_receive;
                Some(Lent::take(slot, active).and_then(|mut lent| Ok(lent.device()?.can_receive())))
            })?;
            package.register_async("send_nec", move |(address, command): (i64, i64)| {
                let (slot, active) = (Arc::clone(&nec.0), Arc::clone(&nec.1));
                async move {
                    Some(
                        async {
                            let command = nec_command(address, command)?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .send_nec(command)
                                .await
                                .map_err(infrared_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("send_raw", move |values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&raw.0), Arc::clone(&raw.1));
                let parsed = arguments(values, 1).and_then(|[list, frequency, duty]| {
                    Ok((
                        timings(&table(list)?)?,
                        carrier(optional_integer(&frequency)?, optional_integer(&duty)?)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (timings, carrier) = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .send_raw(carrier, &timings)
                                .await
                                .map_err(infrared_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async_with("receive", move |lua, values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&receive.0), Arc::clone(&receive.1));
                // The table a raw frame comes back in is made before the wait,
                // while the Lua context is at hand.
                let table = lua.create_table();
                async move {
                    Some(
                        async {
                            let table = table?;
                            let [timeout_ms] = arguments(values, 0)?;
                            let timeout = match optional_integer(&timeout_ms)? {
                                None => None,
                                Some(milliseconds) => Some(Duration::from_millis(
                                    u64::try_from(milliseconds).map_err(|_| {
                                        Error::runtime("a timeout is milliseconds, 0 or more")
                                    })?,
                                )),
                            };
                            let mut lent = Lent::take(&slot, &active)?;
                            let device = lent.device()?;
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
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
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
        let package = InfraredPackage::new(Some(infrared));
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
    fn a_board_without_infrared_reports_none() {
        let package = InfraredPackage::<TestInfrared>::new(None);
        assert!(run(
            &package,
            "local ir = require('infrared')\n\
             local ok, err = ir.send_nec(1, 2)\n\
             return not ir.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = InfraredPackage::new(Some(TestInfrared::default()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
