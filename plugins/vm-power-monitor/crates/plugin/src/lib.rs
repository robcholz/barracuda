//! Lua package over the Board's power monitor, which the `power` Plugin
//! owns.

#![no_std]

extern crate alloc;

use alloc::string::String;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_power_plugin::{Power, PowerMeasurement};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Publishes the `power` Plugin's power monitor to Lua.
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

struct PowerMonitorPackage {
    power: Power,
    /// Whether a Lua handle took the monitor; one may per boot.
    taken: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
}

impl PowerMonitorPackage {
    fn new(power: Power) -> Self {
        Self {
            power,
            taken: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Package for PowerMonitorPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let present = self.power.is_present();
        let available_taken = Arc::clone(&self.taken);
        let available_active = Arc::clone(&self.active);
        let open_power = self.power.clone();
        let open_taken = Arc::clone(&self.taken);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("power_monitor", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && present
                    && !available_taken.load(Ordering::Acquire)))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime(
                        "power-monitor package has been revoked",
                    )));
                }
                if !present || open_taken.swap(true, Ordering::AcqRel) {
                    return Some(Err(Error::runtime(
                        "built-in power monitor is unavailable or already open",
                    )));
                }
                Some(lua.create_userdata(PowerMonitorHandle::new(
                    open_power.clone(),
                    Arc::clone(&open_active),
                )))
            })
        })
    }
}

impl LuaPackage for PowerMonitorPackage {
    fn name(&self) -> &'static str {
        "power_monitor"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        Some(barracuda_vm_plugin::PackageInterface::BoardSpecific {
            purpose: "读取电源数据",
        })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct PowerMonitorHandle {
    power: Option<Power>,
    active: Arc<AtomicBool>,
}

impl PowerMonitorHandle {
    fn new(power: Power, active: Arc<AtomicBool>) -> Self {
        Self {
            power: Some(power),
            active,
        }
    }

    fn close(&mut self) {
        self.power.take();
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
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.power.is_some()
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
        ensure_active(&self.active)?;
        self.power
            .as_ref()
            .ok_or_else(|| Error::runtime("power-monitor handle is closed"))?
            .measure()
            .map_err(|error| {
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

    fn install(monitor: Option<TestMonitor>) -> Lua {
        let package = PowerMonitorPackage::new(Power::from_monitor(monitor));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        lua
    }

    #[test]
    fn lua_measures_the_builtin_monitor_once() {
        let mut lua = install(Some(TestMonitor {
            result: Ok(PowerMeasurement {
                bus_microvolts: 5_000_000,
                shunt_nanovolts: -1_000_000,
                current_microamps: -200_000,
                power_microwatts: -1_000_000,
            }),
        }));

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nassert(monitors.available())\nlocal monitor <close> = monitors.open()\nassert(not monitors.available())\nlocal again, err = monitors.open()\nassert(again == nil and err:find('already open'))\nlocal bus_uv, shunt_nv, current_ua, power_uw = monitor:measure()\nreturn monitor:is_open() and bus_uv == 5000000 and shunt_nv == -1000000 and current_ua == -200000 and power_uw == -1000000")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn measurement_failure_returns_an_error() {
        let mut lua = install(Some(TestMonitor { result: Err(()) }));

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nlocal monitor <close> = monitors.open()\nlocal bus_uv, err = monitor:measure()\nreturn bus_uv == nil and err:find('power%-monitor measurement failed: BusFault') ~= nil and monitor:is_open()")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn closed_handle_rejects_measurement() {
        let mut lua = install(Some(TestMonitor {
            result: Ok(PowerMeasurement {
                bus_microvolts: 0,
                shunt_nanovolts: 0,
                current_microamps: 0,
                power_microwatts: 0,
            }),
        }));

        let result: bool = lua
            .load("local monitor = require('power_monitor').open()\nmonitor:close()\nlocal bus_uv, err = monitor:measure()\nreturn not monitor:is_open() and bus_uv == nil and err:find('closed') ~= nil")
            .eval()
            .expect("measure power");
        assert!(result);
    }

    #[test]
    fn unavailable_board_reports_no_monitor() {
        let mut lua = install(None);

        let result: bool = lua
            .load("local monitors = require('power_monitor')\nlocal monitor, err = monitors.open()\nreturn not monitors.available() and monitor == nil and err:find('unavailable') ~= nil")
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
        let power = Power::from_monitor(Some(Gauge));
        let package = PowerMonitorPackage::new(power.clone());
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        let open: bool = lua
            .load("monitor = require('power_monitor').open()\nreturn monitor:is_open()")
            .eval()
            .expect("open the monitor");
        assert!(open);
        assert_eq!(power.battery().map(|battery| battery.percent), Some(76));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package =
            PowerMonitorPackage::new(Power::from_monitor(Some(TestMonitor { result: Err(()) })));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
