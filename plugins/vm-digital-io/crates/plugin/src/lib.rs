//! Lua package for the Board's built-in relays and digital inputs.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::digital_io::{DigitalIo, DigitalIoPeripheral, Levels};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result, Table, Trailing,
    Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's digital I/O banks and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct DigitalIoPlugin<Device> {
    banks: Vec<(&'static str, Device)>,
}

impl<Device> DigitalIoPlugin<Device> {
    /// Takes the built-in banks from the generated Board capability set, the
    /// primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: DigitalIoPeripheral<DigitalIo = Device>,
    {
        Self {
            banks: config::peripherals::DIGITAL_IO_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_digital_io(*instance)
                        .map(|bank| (*name, bank))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for DigitalIoPlugin<Device>
where
    Device: DigitalIo + Send + 'static,
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
            .register(DigitalIoPackage::new(core::mem::take(&mut self.banks)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct DigitalIoPackage<Device> {
    banks: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> DigitalIoPackage<Device> {
    fn new(banks: Vec<(&'static str, Device)>) -> Self {
        Self {
            banks: BoardDevices::new(banks),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Runs `operation` on the bank `name`, or the primary one, lent out of the
/// package so the expander's bus transfers run outside its lock.
fn with_bank<Device, T>(
    banks: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
    operation: impl FnOnce(&mut Device) -> Result<T>,
) -> Result<T> {
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("digital_io package has been revoked"));
    }
    let mut bank = banks.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("digital I/O device `{name}` is unavailable"),
            None => String::from("the Board has no relays or digital inputs"),
        })
    })?;
    operation(&mut bank)
}

fn bank_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("digital I/O operation failed: {error:?}"))
}

/// The zero-based channel of a one-based Lua `index`, checked against
/// `count`.
fn channel(index: i64, count: usize, what: &str) -> Result<usize> {
    usize::try_from(index)
        .ok()
        .and_then(|index| index.checked_sub(1))
        .filter(|channel| *channel < count)
        .ok_or_else(|| Error::runtime(alloc::format!("{what} {index} does not exist")))
}

fn fill(table: &Table, levels: Levels, count: usize) -> Result<()> {
    for index in 0..count {
        table.set(
            i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1),
            levels.is_on(index),
        )?;
    }
    Ok(())
}

impl<Device> Package for DigitalIoPackage<Device>
where
    Device: DigitalIo + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.banks.clone(), Arc::clone(&self.active));
        let list = self.banks.clone();
        let counts = (self.banks.clone(), Arc::clone(&self.active));
        let set = (self.banks.clone(), Arc::clone(&self.active));
        let get = (self.banks.clone(), Arc::clone(&self.active));
        let outputs = (self.banks.clone(), Arc::clone(&self.active));
        let read = (self.banks.clone(), Arc::clone(&self.active));
        let inputs = (self.banks.clone(), Arc::clone(&self.active));
        lua.register_lib("digital_io", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (banks, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && banks.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("counts", move |name: Variadic<String>| {
                let (slot, active) = &counts;
                Some(with_bank(
                    slot,
                    active,
                    name.first().map(String::as_str),
                    |bank| {
                        Ok((
                            i64::try_from(bank.output_count()).unwrap_or(i64::MAX),
                            i64::try_from(bank.input_count()).unwrap_or(i64::MAX),
                        ))
                    },
                ))
            })?;
            package.register(
                "set",
                move |Trailing((index, on), name): Trailing<(i64, bool), String>| {
                    let (slot, active) = &set;
                    Some(with_bank(slot, active, name.as_deref(), |bank| {
                        let channel = channel(index, bank.output_count(), "output")?;
                        bank.set_output(channel, on).map_err(bank_error)?;
                        Ok(true)
                    }))
                },
            )?;
            package.register(
                "get",
                move |Trailing((index,), name): Trailing<(i64,), String>| {
                    let (slot, active) = &get;
                    Some(with_bank(slot, active, name.as_deref(), |bank| {
                        let channel = channel(index, bank.output_count(), "output")?;
                        Ok(bank.outputs().is_on(channel))
                    }))
                },
            )?;
            package.register_with("outputs", move |lua, name: Variadic<String>| {
                let (slot, active) = &outputs;
                Some((|| {
                    let table = lua.create_table()?;
                    with_bank(slot, active, name.first().map(String::as_str), |bank| {
                        fill(&table, bank.outputs(), bank.output_count())
                    })?;
                    Ok(table)
                })())
            })?;
            package.register(
                "read",
                move |Trailing((index,), name): Trailing<(i64,), String>| {
                    let (slot, active) = &read;
                    Some(with_bank(slot, active, name.as_deref(), |bank| {
                        let channel = channel(index, bank.input_count(), "input")?;
                        Ok(bank.read_inputs().map_err(bank_error)?.is_on(channel))
                    }))
                },
            )?;
            package.register_with("inputs", move |lua, name: Variadic<String>| {
                let (slot, active) = &inputs;
                Some((|| {
                    let table = lua.create_table()?;
                    with_bank(slot, active, name.first().map(String::as_str), |bank| {
                        let levels = bank.read_inputs().map_err(bank_error)?;
                        fill(&table, levels, bank.input_count())
                    })?;
                    Ok(table)
                })())
            })
        })
    }
}

impl<Device> LuaPackage for DigitalIoPackage<Device>
where
    Device: DigitalIo + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "digital_io"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.banks.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "Switch relays and read inputs",
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

    /// Four relays and eight inputs; input n reads on when relay n is on.
    #[derive(Default)]
    struct TestBank {
        outputs: Levels,
    }

    impl DigitalIo for TestBank {
        type Error = core::convert::Infallible;

        fn output_count(&self) -> usize {
            4
        }

        fn input_count(&self) -> usize {
            8
        }

        fn set_output(&mut self, index: usize, on: bool) -> core::result::Result<(), Self::Error> {
            self.outputs = self.outputs.with(index, on);
            Ok(())
        }

        fn outputs(&self) -> Levels {
            self.outputs
        }

        fn read_inputs(&mut self) -> core::result::Result<Levels, Self::Error> {
            Ok(self.outputs)
        }
    }

    fn run(package: &DigitalIoPackage<TestBank>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_switches_relays_and_reads_inputs_by_label() {
        let package = DigitalIoPackage::new(std::vec![("main", TestBank::default())]);
        assert!(run(
            &package,
            "local io = require('digital_io')\n\
             local outputs, inputs = io.counts()\n\
             assert(io.available() and outputs == 4 and inputs == 8)\n\
             assert(io.set(1, true) and io.set(4, true))\n\
             assert(io.get(1) and not io.get(2))\n\
             local all = io.outputs()\n\
             assert(#all == 4 and all[1] and all[4] and not all[3])\n\
             assert(io.read(4) and not io.read(5))\n\
             assert(#io.inputs() == 8)\n\
             local ok, err = io.set(5, true)\n\
             return ok == nil and err:find('output 5') ~= nil",
        ));
    }

    #[test]
    fn lua_names_a_bank_and_defaults_to_the_primary() {
        let package = DigitalIoPackage::new(std::vec![
            ("main", TestBank::default()),
            ("side", TestBank::default()),
        ]);
        assert!(run(
            &package,
            "local io = require('digital_io')\n\
             local names = io.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(io.set(2, true, 'side'))\n\
             assert(io.get(2, 'side') and not io.get(2))\n\
             assert(io.read(2, 'side') and io.outputs('side')[2] and #io.inputs('side') == 8)\n\
             local ok, err = io.counts('missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
    }

    #[test]
    fn a_board_without_bank_reports_none() {
        let package = DigitalIoPackage::<TestBank>::new(Vec::new());
        assert!(run(
            &package,
            "local io = require('digital_io')\n\
             local ok, err = io.set(1, true)\n\
             return not io.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = DigitalIoPackage::new(std::vec![("main", TestBank::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
