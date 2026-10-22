//! Lua package for the Board's built-in vibration motors.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::vibration::{Pulse, Strength, Vibration, VibrationPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most steps one pattern may hold.
const MAX_STEPS: usize = 64;

/// Owns the Board's vibration motors and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct VibrationPlugin<Device> {
    motors: Vec<(&'static str, Device)>,
}

impl<Device> VibrationPlugin<Device> {
    /// Takes the built-in motors from the generated Board capability set, the
    /// primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: VibrationPeripheral<Vibration = Device>,
    {
        Self {
            motors: config::peripherals::VIBRATION_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_vibration(*instance)
                        .map(|motor| (*name, motor))
                })
                .collect(),
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
            .register(VibrationPackage::new(core::mem::take(&mut self.motors)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Lends the motor `name`, or the primary one, to one call; it goes back when
/// the call ends or is cancelled.
fn lend<Device>(
    motors: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
) -> Result<LentDevice<Device>> {
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("vibration package has been revoked"));
    }
    motors.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("vibration motor `{name}` is unavailable or busy"),
            None => String::from("built-in vibration motor is unavailable or busy"),
        })
    })
}

struct VibrationPackage<Device> {
    motors: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> VibrationPackage<Device> {
    fn new(motors: Vec<(&'static str, Device)>) -> Self {
        Self {
            motors: BoardDevices::new(motors),
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

/// A motor name argument, or `None` for the primary motor when it is nil.
fn device_name(value: LuaValue) -> Result<Option<String>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(name) => Ok(Some(name)),
        _ => Err(Error::runtime("expected a vibration motor name")),
    }
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
        let available = (self.motors.clone(), Arc::clone(&self.active));
        let list = self.motors.clone();
        let variable = (self.motors.clone(), Arc::clone(&self.active));
        let set = (self.motors.clone(), Arc::clone(&self.active));
        let stop = (self.motors.clone(), Arc::clone(&self.active));
        let pulse = (self.motors.clone(), Arc::clone(&self.active));
        let run = (self.motors.clone(), Arc::clone(&self.active));
        lua.register_lib("vibration", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (motors, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && motors.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("variable", move |name: Variadic<String>| {
                let (motors, active) = &variable;
                Some(
                    lend(motors, active, name.first().map(String::as_str))
                        .map(|lent| lent.has_variable_strength()),
                )
            })?;
            package.register("set", move |values: Variadic<LuaValue>| {
                let (motors, active) = &set;
                Some((|| {
                    let [percent, name] = arguments(values, 1)?;
                    let strength = strength(Some(integer(&percent)?))?;
                    let name = device_name(name)?;
                    let mut lent = lend(motors, active, name.as_deref())?;
                    lent.set(strength).map_err(motor_error)?;
                    Ok(true)
                })())
            })?;
            package.register("stop", move |name: Variadic<String>| {
                let (motors, active) = &stop;
                Some((|| {
                    let mut lent = lend(motors, active, name.first().map(String::as_str))?;
                    lent.set(Strength::OFF).map_err(motor_error)?;
                    Ok(true)
                })())
            })?;
            package.register_async("pulse", move |values: Variadic<LuaValue>| {
                let (motors, active) = (pulse.0.clone(), Arc::clone(&pulse.1));
                let parsed = arguments(values, 1).and_then(|[duration, percent, name]| {
                    Ok((
                        Pulse::new(milliseconds(integer(&duration)?)?, 0),
                        strength(optional_integer(&percent)?)?,
                        device_name(name)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (step, strength, name) = parsed?;
                            let mut lent = lend(&motors, &active, name.as_deref())?;
                            lent.pulse(&[step], strength).await.map_err(motor_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("pattern", move |values: Variadic<LuaValue>| {
                let (motors, active) = (run.0.clone(), Arc::clone(&run.1));
                let parsed = arguments(values, 1).and_then(|[steps, percent, name]| {
                    Ok((
                        pattern(&table(steps)?)?,
                        strength(optional_integer(&percent)?)?,
                        device_name(name)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (steps, strength, name) = parsed?;
                            let mut lent = lend(&motors, &active, name.as_deref())?;
                            lent.pulse(&steps, strength).await.map_err(motor_error)?;
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
        (!self.motors.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
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
    use core::cell::RefCell;
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
        let package = VibrationPackage::new(std::vec![("main", motor)]);
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
    fn lua_names_a_motor_and_defaults_to_the_primary() {
        let (main, side) = (TestMotor::default(), TestMotor::default());
        let (main_log, side_log) = (Arc::clone(&main.strengths), Arc::clone(&side.strengths));
        let package = VibrationPackage::new(std::vec![("main", main), ("side", side)]);
        assert!(run(
            &package,
            "local vibration = require('vibration')\n\
             local names = vibration.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(vibration.set(40))\n\
             assert(vibration.set(70, 'side'))\n\
             assert(vibration.pulse(10, nil, 'side'))\n\
             local ok, err = vibration.stop('missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
        let strengths = |log: &Arc<critical_section::Mutex<RefCell<Vec<u8>>>>| {
            critical_section::with(|section| log.borrow(section).borrow().clone())
        };
        assert_eq!(strengths(&main_log), [40]);
        assert_eq!(strengths(&side_log), [70, 100, 0]);
    }

    #[test]
    fn a_board_without_motor_reports_none() {
        let package = VibrationPackage::<TestMotor>::new(Vec::new());
        assert!(run(
            &package,
            "local vibration = require('vibration')\n\
             local ok, err = vibration.pulse(10)\n\
             return not vibration.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = VibrationPackage::new(std::vec![("main", TestMotor::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
