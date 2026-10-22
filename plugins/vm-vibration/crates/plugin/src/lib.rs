//! Lua package for the Board's primary built-in vibration motor.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::vec::Vec;
use barracuda_board_hal::vibration::{Pulse, Strength, Vibration, VibrationPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result, Table, Variadic,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most steps one pattern may hold.
const MAX_STEPS: usize = 64;

/// Owns the Board's primary vibration motor and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct VibrationPlugin<Device> {
    motor: Option<Device>,
}

impl<Device> VibrationPlugin<Device> {
    /// Takes the built-in motor from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: VibrationPeripheral<Vibration = Device>,
    {
        Self {
            motor: config::peripherals::VIBRATION_DEVICE
                .and_then(|instance| context.hal.peripherals.take_vibration(instance)),
        }
    }
}

impl<Device> Plugin for VibrationPlugin<Device>
where
    Device: Vibration + Send + 'static,
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
            .register(VibrationPackage::new(self.motor.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The motor while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The motor taken out of its slot for one call; it goes back when the call
/// ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("vibration package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime(
                "built-in vibration motor is unavailable or busy",
            )),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in vibration motor is unavailable or busy"))
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

struct VibrationPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    motor: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> VibrationPackage<Device> {
    fn new(motor: Option<Device>) -> Self {
        Self {
            present: motor.is_some(),
            motor: Arc::new(critical_section::Mutex::new(RefCell::new(motor))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn strength(percent: Option<i64>) -> Result<Strength> {
    match percent {
        None => Ok(Strength::MAX),
        Some(percent) => u8::try_from(percent)
            .ok()
            .filter(|percent| *percent <= 100)
            .map(Strength::percent)
            .ok_or_else(|| Error::runtime("a strength is 0 through 100")),
    }
}

fn milliseconds(value: i64) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::runtime("a duration is milliseconds, 0 or more"))
}

/// Reads `{ {on_ms, off_ms}, ... }`.
fn pattern(steps: &Table) -> Result<Vec<Pulse>> {
    let length = steps.len()?;
    if length > MAX_STEPS {
        return Err(Error::runtime("a pattern holds at most 64 steps"));
    }
    let mut pattern = Vec::with_capacity(length);
    for index in 1..=length {
        let step: Table = steps.get(i64::try_from(index).unwrap_or(i64::MAX))?;
        pattern.push(Pulse::new(
            milliseconds(step.get(1i64)?)?,
            milliseconds(step.get(2i64)?)?,
        ));
    }
    Ok(pattern)
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

/// An integer argument that must be given.
fn integer(value: &LuaValue) -> Result<i64> {
    optional_integer(value)?.ok_or_else(|| Error::runtime("expected an integer argument"))
}

/// A table argument.
fn table(value: LuaValue) -> Result<Table> {
    match value {
        LuaValue::Table(table) => Ok(table),
        _ => Err(Error::runtime("expected a table argument")),
    }
}

fn motor_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("vibration operation failed: {error:?}"))
}

impl<Device> Package for VibrationPackage<Device>
where
    Device: Vibration + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (Arc::clone(&self.motor), Arc::clone(&self.active));
        let variable = (Arc::clone(&self.motor), Arc::clone(&self.active));
        let set = (Arc::clone(&self.motor), Arc::clone(&self.active));
        let stop = (Arc::clone(&self.motor), Arc::clone(&self.active));
        let pulse = (Arc::clone(&self.motor), Arc::clone(&self.active));
        let run = (Arc::clone(&self.motor), Arc::clone(&self.active));
        lua.register_lib("vibration", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register("variable", move |(): ()| {
                let (slot, active) = &variable;
                Some(
                    Lent::take(slot, active)
                        .and_then(|mut lent| Ok(lent.device()?.has_variable_strength())),
                )
            })?;
            package.register("set", move |percent: i64| {
                let (slot, active) = &set;
                Some((|| {
                    let strength = strength(Some(percent))?;
                    let mut lent = Lent::take(slot, active)?;
                    lent.device()?.set(strength).map_err(motor_error)?;
                    Ok(true)
                })())
            })?;
            package.register("stop", move |(): ()| {
                let (slot, active) = &stop;
                Some((|| {
                    let mut lent = Lent::take(slot, active)?;
                    lent.device()?.set(Strength::OFF).map_err(motor_error)?;
                    Ok(true)
                })())
            })?;
            package.register_async("pulse", move |values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&pulse.0), Arc::clone(&pulse.1));
                let parsed = arguments(values, 1).and_then(|[duration, percent]| {
                    Ok((
                        Pulse::new(milliseconds(integer(&duration)?)?, 0),
                        strength(optional_integer(&percent)?)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (step, strength) = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .pulse(&[step], strength)
                                .await
                                .map_err(motor_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("pattern", move |values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&run.0), Arc::clone(&run.1));
                let parsed = arguments(values, 1).and_then(|[steps, percent]| {
                    Ok((
                        pattern(&table(steps)?)?,
                        strength(optional_integer(&percent)?)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (steps, strength) = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .pulse(&steps, strength)
                                .await
                                .map_err(motor_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })
        })
    }
}

impl<Device> LuaPackage for VibrationPackage<Device>
where
    Device: Vibration + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "vibration"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "振动提醒",
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

    /// Records each strength it is set to.
    struct TestMotor {
        strengths: Arc<critical_section::Mutex<RefCell<Vec<u8>>>>,
    }

    impl Default for TestMotor {
        fn default() -> Self {
            Self {
                strengths: Arc::new(critical_section::Mutex::new(RefCell::new(Vec::new()))),
            }
        }
    }

    impl Vibration for TestMotor {
        type Error = Infallible;

        fn has_variable_strength(&self) -> bool {
            true
        }

        fn set(&mut self, strength: Strength) -> core::result::Result<(), Infallible> {
            critical_section::with(|section| {
                self.strengths
                    .borrow(section)
                    .borrow_mut()
                    .push(strength.get());
            });
            Ok(())
        }

        async fn pulse(
            &mut self,
            pattern: &[Pulse],
            strength: Strength,
        ) -> core::result::Result<(), Infallible> {
            for _ in pattern {
                self.set(strength)?;
                self.set(Strength::OFF)?;
            }
            Ok(())
        }
    }

    fn run(package: &VibrationPackage<TestMotor>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_sets_pulses_and_patterns() {
        let motor = TestMotor::default();
        let strengths = Arc::clone(&motor.strengths);
        let package = VibrationPackage::new(Some(motor));
        assert!(run(
            &package,
            "local vibration = require('vibration')\n\
             assert(vibration.available() and vibration.variable())\n\
             assert(vibration.set(40))\n\
             assert(vibration.stop())\n\
             assert(vibration.pulse(200))\n\
             assert(vibration.pattern({ {100, 50}, {100, 0} }, 60))\n\
             local ok, err = vibration.set(101)\n\
             return ok == nil and err:find('strength') ~= nil",
        ));
        let strengths =
            critical_section::with(|section| strengths.borrow(section).borrow().clone());
        assert_eq!(strengths, [40, 0, 100, 0, 60, 0, 60, 0]);
    }

    #[test]
    fn a_board_without_motor_reports_none() {
        let package = VibrationPackage::<TestMotor>::new(None);
        assert!(run(
            &package,
            "local vibration = require('vibration')\n\
             local ok, err = vibration.pulse(10)\n\
             return not vibration.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = VibrationPackage::new(Some(TestMotor::default()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
