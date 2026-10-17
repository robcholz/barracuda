//! Lua package for the Board's primary attached power monitor, and the
//! battery's charge for other Plugins.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{boxed::Box, rc::Rc, string::String};
use barracuda_board_hal::power::{PowerMeasurement, PowerMonitor, PowerMonitorPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

pub use barracuda_board_hal::power::BatteryState;

/// The Board's power monitor, shared by the Lua package and [`PowerStatus`].
type SharedMonitor<Monitor> = Arc<critical_section::Mutex<RefCell<Option<Monitor>>>>;

/// The battery's charge, read from the Board's power monitor when it is a
/// fuel gauge. Published by the `power-monitor` Plugin.
pub struct PowerStatus {
    battery: Box<dyn Fn() -> Option<BatteryState>>,
}

impl PowerStatus {
    /// Reads the battery's charge now; `None` on a Board without a battery
    /// fuel gauge or when the reading fails.
    #[must_use]
    pub fn battery(&self) -> Option<BatteryState> {
        (self.battery)()
    }
}

/// Owns the Board's primary power-monitor capability and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct PowerMonitorPlugin<Monitor> {
    monitor: Option<Monitor>,
}

impl<Monitor> PowerMonitorPlugin<Monitor> {
    /// Takes the built-in power monitor from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: PowerMonitorPeripheral<PowerMonitor = Monitor>,
    {
        Self {
            monitor: config::peripherals::POWER_MONITOR_DEVICE
                .and_then(|instance| context.hal.peripherals.take_power_monitor(instance)),
        }
    }
}

impl<Monitor> Plugin for PowerMonitorPlugin<Monitor>
where
    Monitor: PowerMonitor + Send + 'static,
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
        let package = PowerMonitorPackage::new(self.monitor.take());
        let monitor = Arc::clone(&package.monitor);
        context.provide(Rc::new(PowerStatus {
            battery: Box::new(move || battery(&monitor)),
        }))?;
        let registration = registry
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Reads the battery's charge from `monitor`.
fn battery<Monitor: PowerMonitor>(monitor: &SharedMonitor<Monitor>) -> Option<BatteryState> {
    critical_section::with(|section| {
        monitor
            .borrow(section)
            .borrow_mut()
            .as_mut()?
            .battery()
            .ok()
            .flatten()
    })
}

struct PowerMonitorPackage<Monitor> {
    monitor: SharedMonitor<Monitor>,
    /// Whether a Lua handle took the monitor; one may per boot.
    taken: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
}

impl<Monitor> PowerMonitorPackage<Monitor> {
    fn new(monitor: Option<Monitor>) -> Self {
        Self {
            monitor: Arc::new(critical_section::Mutex::new(RefCell::new(monitor))),
            taken: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(true)),
        }
    }

    fn present(&self) -> bool {
        critical_section::with(|section| self.monitor.borrow(section).borrow().is_some())
    }
}

impl<Monitor> Package for PowerMonitorPackage<Monitor>
where
    Monitor: PowerMonitor + Send + 'static,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let present = self.present();
        let available_taken = Arc::clone(&self.taken);
        let available_active = Arc::clone(&self.active);
        let open_monitor = Arc::clone(&self.monitor);
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
                    Arc::clone(&open_monitor),
                    Arc::clone(&open_active),
                )))
            })
        })
    }
}

impl<Monitor> LuaPackage for PowerMonitorPackage<Monitor>
where
    Monitor: PowerMonitor + Send + 'static,
{
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

struct PowerMonitorHandle<Monitor> {
    monitor: Option<SharedMonitor<Monitor>>,
    active: Arc<AtomicBool>,
}

impl<Monitor> PowerMonitorHandle<Monitor> {
    fn new(monitor: SharedMonitor<Monitor>, active: Arc<AtomicBool>) -> Self {
        Self {
            monitor: Some(monitor),
            active,
        }
    }

    fn close(&mut self) {
        self.monitor.take();
    }
}

impl<Monitor> UserData for PowerMonitorHandle<Monitor>
where
    Monitor: PowerMonitor + Send + 'static,
{
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

impl<Monitor> PowerMonitorHandle<Monitor>
where
    Monitor: PowerMonitor,
{
    fn measure(&mut self) -> Result<PowerMeasurement> {
        ensure_active(&self.active)?;
        let monitor = self
            .monitor
            .as_ref()
            .ok_or_else(|| Error::runtime("power-monitor handle is closed"))?;
        critical_section::with(|section| {
            monitor
                .borrow(section)
                .borrow_mut()
                .as_mut()
                .ok_or_else(|| Error::runtime("power-monitor handle is closed"))?
                .measure()
                .map_err(|error| {
                    Error::runtime(alloc::format!(
                        "power-monitor measurement failed: {error:?}"
                    ))
                })
        })
    }
}

impl<Monitor> Drop for PowerMonitorHandle<Monitor> {
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
        let package = PowerMonitorPackage::new(monitor);
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
    fn battery_reads_while_lua_holds_the_monitor() {
        let package = PowerMonitorPackage::new(Some(Gauge));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        let open: bool = lua
            .load("monitor = require('power_monitor').open()\nreturn monitor:is_open()")
            .eval()
            .expect("open the monitor");
        assert!(open);
        assert_eq!(
            battery(&package.monitor),
            Some(BatteryState {
                percent: 76,
                charging: true,
            })
        );
        // A plain monitor has no battery.
        let plain = PowerMonitorPackage::new(Some(TestMonitor { result: Err(()) }));
        assert_eq!(battery(&plain.monitor), None);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = PowerMonitorPackage::new(Some(TestMonitor { result: Err(()) }));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
