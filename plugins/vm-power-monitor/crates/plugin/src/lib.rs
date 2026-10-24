//! Lua package over the Board's power monitors, which the `power` Plugin
//! owns.

#![no_std]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_power_plugin::{Monitor, Power, PowerMeasurement};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// The Lua package name.
const PACKAGE: &str = "power_monitor";

/// Publishes the `power` Plugin's power monitors to Lua.
#[barracuda_plugin::macros::plugin]
pub struct PowerMonitorPlugin;

impl PowerMonitorPlugin {
    /// Creates the unregistered package.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for PowerMonitorPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>("vm")?;
        let power = context.require::<Power>("power")?;
        let registration = registry
            .register(PowerMonitorPackage::new(Power::clone(&power)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Each monitor's Board name, the primary first, and whether a Lua handle
/// took it; one may per monitor and boot.
type Taken = Arc<Vec<(&'static str, AtomicBool)>>;

struct PowerMonitorPackage {
    power: Power,
    taken: Taken,
    active: Arc<AtomicBool>,
}

impl PowerMonitorPackage {
    fn new(power: Power) -> Self {
        let taken = power
            .names()
            .map(|name| (name, AtomicBool::new(false)))
            .collect();
        Self {
            power,
            taken: Arc::new(taken),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// The position of the monitor named `name`, or of the primary one.
fn position(taken: &[(&'static str, AtomicBool)], name: Option<&str>) -> Option<usize> {
    match name {
        None => (!taken.is_empty()).then_some(0),
        Some(name) => taken.iter().position(|(monitor, _)| *monitor == name),
    }
}

impl Package for PowerMonitorPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (Arc::clone(&self.taken), Arc::clone(&self.active));
        let list = Arc::clone(&self.taken);
        let open = (
            self.power.clone(),
            Arc::clone(&self.taken),
            Arc::clone(&self.active),
        );
        lua.register_lib(PACKAGE, move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (taken, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && position(taken, name.first().map(String::as_str))
                        .is_some_and(|index| {
                            !taken[index].1.load(Ordering::Acquire)
                        })))
            })?;
            package.register_with("list", move |lua, (): ()| {
                Some((|| {
                    let names = lua.create_table()?;
                    for (number, (name, _)) in (1_i64..).zip(list.iter()) {
                        names.set(number, *name)?;
                    }
                    Ok(names)
                })())
            })?;
            package.register_with("open", move |lua, name: Variadic<String>| {
                let (power, taken, active) = &open;
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime(
                        "power-monitor package has been revoked",
                    )));
                }
                let name = name.first().map(String::as_str);
                let monitor = position(taken, name)
                    .filter(|&index| !taken[index].1.swap(true, Ordering::AcqRel))
                    .and_then(|index| power.monitor(Some(taken[index].0)));
                Some(match monitor {
                    Some(monitor) => {
                        lua.create_userdata(PowerMonitorHandle::new(monitor, Arc::clone(active)))
                    }
                    None => Err(Error::runtime(match name {
                        Some(name) => {
                            alloc::format!("power monitor `{name}` is unavailable or already open")
                        }
                        None => {
                            String::from("built-in power monitor is unavailable or already open")
                        }
                    })),
                })
            })
        })
    }
}

impl LuaPackage for PowerMonitorPackage {
    fn name(&self) -> &'static str {
        PACKAGE
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.power
            .is_present()
            .then_some(barracuda_vm_plugin::PackageInterface::BoardSpecific {
                purpose: "Read power data",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct PowerMonitorHandle {
    monitor: Option<Monitor>,
    active: Arc<AtomicBool>,
}

impl PowerMonitorHandle {
    fn new(monitor: Monitor, active: Arc<AtomicBool>) -> Self {
        Self {
            monitor: Some(monitor),
            active,
        }
    }

    fn close(&mut self) {
        self.monitor.take();
    }

    fn monitor(&self) -> Result<&Monitor> {
        ensure_active(&self.active)?;
        self.monitor
            .as_ref()
            .ok_or_else(|| Error::runtime("power-monitor handle is closed"))
    }
}

impl UserData for PowerMonitorHandle {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method_mut("measure", |handle, (): ()| {
            Some(
                handle
                    .measure()
                    .and_then(|measurement| measurement_values(&measurement)),
            )
        });
        methods.add_method("battery", |handle, (): ()| match handle.monitor() {
            Ok(monitor) => monitor
                .battery()
                .map(|battery| Ok((i64::from(battery.percent), battery.charging))),
            Err(error) => Some(Err(error)),
        });
        methods.add_method("name", |handle, (): ()| {
            Some(handle.monitor().map(Monitor::name))
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.monitor.is_some()
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

impl PowerMonitorHandle {
    fn measure(&mut self) -> Result<PowerMeasurement> {
        self.monitor()?.measure().map_err(|error| {
            Error::runtime(alloc::format!(
                "power-monitor measurement failed: {}",
                error.0
            ))
        })
    }
}

impl Drop for PowerMonitorHandle {
    fn drop(&mut self) {
        self.close();
    }
}

fn measurement_values(measurement: &PowerMeasurement) -> Result<(i64, i64, i64, i64)> {
    let bus_microvolts = i64::try_from(measurement.bus_microvolts)
        .map_err(|_| Error::runtime("power-monitor bus voltage is out of range"))?;
    Ok((
        bus_microvolts,
        measurement.shunt_nanovolts,
        measurement.current_microamps,
        measurement.power_microwatts,
    ))
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("power-monitor package has been revoked"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use barracuda_board_hal::power::{BatteryState, PowerMonitor};

    #[derive(Debug)]
    struct BusFault;

    struct TestMonitor {
        result: core::result::Result<PowerMeasurement, ()>,
    }

    impl PowerMonitor for TestMonitor {
        type Error = BusFault;

        fn measure(&mut self) -> core::result::Result<PowerMeasurement, Self::Error> {
            self.result.map_err(|()| BusFault)
        }
    }

    fn reading(current_microamps: i64) -> TestMonitor {
        TestMonitor {
            result: Ok(PowerMeasurement {
                bus_microvolts: 5_000_000,
                shunt_nanovolts: -1_000_000,
                current_microamps,
                power_microwatts: -1_000_000,
            }),
        }
    }

    fn install(monitors: std::vec::Vec<(&'static str, TestMonitor)>) -> Lua {
        let package = PowerMonitorPackage::new(Power::from_monitors(monitors));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        lua
    }

    #[test]
    fn lua_measures_the_primary_monitor_once() {
        let mut lua = install(std::vec![("battery", reading(-200_000))]);

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nassert(monitors.available())\nlocal monitor <close> = monitors.open()\nassert(not monitors.available())\nlocal again, err = monitors.open()\nassert(again == nil and err:find('already open'))\nlocal bus_uv, shunt_nv, current_ua, power_uw = monitor:measure()\nreturn monitor:is_open() and monitor:name() == 'battery' and bus_uv == 5000000 and shunt_nv == -1000000 and current_ua == -200000 and power_uw == -1000000 and monitor:battery() == nil")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn lua_lists_and_opens_each_monitor_by_name() {
        let mut lua = install(std::vec![
            ("battery", reading(-200_000)),
            ("port-a1", reading(120_000)),
            ("port-b1", reading(30_000)),
        ]);

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nlocal names = monitors.list()\nassert(#names == 3 and names[1] == 'battery' and names[3] == 'port-b1')\nlocal port <close> = assert(monitors.open('port-a1'))\nlocal _, _, current_ua = port:measure()\nassert(current_ua == 120000 and port:name() == 'port-a1')\nassert(not monitors.available('port-a1') and monitors.available('port-b1') and monitors.available())\nlocal missing, err = monitors.open('port-c1')\nlocal primary <close> = monitors.open()\nreturn missing == nil and err:find('port%-c1') ~= nil and primary:name() == 'battery'")
            .eval()
            .expect("measure ports");
        assert!(result);
    }

    #[test]
    fn measurement_failure_returns_an_error() {
        let mut lua = install(std::vec![("battery", TestMonitor { result: Err(()) })]);

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nlocal monitor <close> = monitors.open()\nlocal bus_uv, err = monitor:measure()\nreturn bus_uv == nil and err:find('power%-monitor measurement failed: BusFault') ~= nil and monitor:is_open()")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn closed_handle_rejects_measurement() {
        let mut lua = install(std::vec![("battery", reading(0))]);

        let result: bool = lua
            .load("local monitor = require('power_monitor').open()\nmonitor:close()\nlocal bus_uv, err = monitor:measure()\nreturn not monitor:is_open() and bus_uv == nil and err:find('closed') ~= nil")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn unavailable_board_reports_no_monitor() {
        let package = PowerMonitorPackage::new(Power::from_monitors(std::vec::Vec::<(
            &'static str,
            TestMonitor,
        )>::new()));
        assert!(package.interface().is_none());
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nlocal monitor, err = monitors.open()\nreturn not monitors.available() and #monitors.list() == 0 and monitor == nil and err:find('unavailable') ~= nil")
            .eval()
            .expect("query power monitor");
        assert!(result);
    }

    struct Gauge;

    impl PowerMonitor for Gauge {
        type Error = BusFault;

        fn measure(&mut self) -> core::result::Result<PowerMeasurement, Self::Error> {
            Err(BusFault)
        }

        fn battery(&mut self) -> core::result::Result<Option<BatteryState>, Self::Error> {
            Ok(Some(BatteryState {
                percent: 76,
                charging: true,
            }))
        }
    }

    #[test]
    fn the_battery_reads_while_lua_holds_the_monitor() {
        let power = Power::from_monitors([("battery", Gauge)]);
        let package = PowerMonitorPackage::new(power.clone());
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        let open: bool = lua
            .load("monitor = require('power_monitor').open()\nlocal percent, charging = monitor:battery()\nreturn monitor:is_open() and percent == 76 and charging")
            .eval()
            .expect("open the monitor");
        assert!(open);
        assert_eq!(power.battery().map(|battery| battery.percent), Some(76));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = PowerMonitorPackage::new(Power::from_monitors([(
            "battery",
            TestMonitor { result: Err(()) },
        )]));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
