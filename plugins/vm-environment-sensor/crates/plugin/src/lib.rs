//! Lua package for the Board's primary built-in environment sensor.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::environment::{
    EnvironmentReading, EnvironmentSensor, EnvironmentSensorPeripheral, Quantity,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods, Variadic,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's primary environment sensor and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct EnvironmentSensorPlugin<Device> {
    sensor: Option<Device>,
}

impl<Device> EnvironmentSensorPlugin<Device> {
    /// Takes the built-in environment sensor from the generated Board
    /// capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: EnvironmentSensorPeripheral<EnvironmentSensor = Device>,
    {
        Self {
            sensor: config::peripherals::ENVIRONMENT_SENSOR_DEVICE
                .and_then(|instance| context.hal.peripherals.take_environment_sensor(instance)),
        }
    }
}

impl<Device> Plugin for EnvironmentSensorPlugin<Device>
where
    Device: EnvironmentSensor + Send + 'static,
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
            .register(EnvironmentPackage::new(self.sensor.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The Lua package name.
const PACKAGE: &str = "environment";

struct EnvironmentPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    sensor: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> EnvironmentPackage<Device> {
    fn new(sensor: Option<Device>) -> Self {
        Self {
            present: sensor.is_some(),
            sensor: Arc::new(critical_section::Mutex::new(RefCell::new(sensor))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for EnvironmentPackage<Device>
where
    Device: EnvironmentSensor + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_sensor = Arc::clone(&self.sensor);
        let available_active = Arc::clone(&self.active);
        let open_sensor = Arc::clone(&self.sensor);
        let open_active = Arc::clone(&self.active);
        lua.register_lib(PACKAGE, move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_sensor.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("environment package has been revoked")));
                }
                let sensor = critical_section::with(|section| {
                    open_sensor.borrow(section).borrow_mut().take()
                });
                Some(match sensor {
                    Some(sensor) => lua
                        .create_userdata(EnvironmentHandle::new(sensor, Arc::clone(&open_active))),
                    None => Err(Error::runtime(
                        "built-in environment sensor is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for EnvironmentPackage<Device>
where
    Device: EnvironmentSensor + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        PACKAGE
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "读取环境传感器",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct EnvironmentHandle<Device> {
    sensor: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> EnvironmentHandle<Device> {
    fn new(sensor: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            sensor: Some(sensor),
            active,
        }
    }

    fn close(&mut self) {
        self.sensor.take();
    }
}

impl<Device> UserData for EnvironmentHandle<Device>
where
    Device: EnvironmentSensor + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("quantities", |handle, (): ()| Some(handle.quantity_names()));
        methods.add_async_method("read", |handle, names: Variadic<String>| async move {
            Some(read(handle, names).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.sensor.is_some()
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

impl<Device> EnvironmentHandle<Device>
where
    Device: EnvironmentSensor,
{
    fn sensor(&self) -> Result<&Device> {
        ensure_active(&self.active)?;
        self.sensor
            .as_ref()
            .ok_or_else(|| Error::runtime("environment sensor handle is closed"))
    }

    fn quantity_names(&self) -> Result<Variadic<&'static str>> {
        let quantities = self.sensor()?.quantities();
        Ok(quantities
            .iter()
            .map(Quantity::name)
            .collect::<Vec<_>>()
            .into())
    }
}

impl<Device> Drop for EnvironmentHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Reads one sample and returns the requested quantities' values in order;
/// with no names, every quantity the sensor reports.
async fn read<Device>(
    handle: UserDataHandle<EnvironmentHandle<Device>>,
    names: Variadic<String>,
) -> Result<Variadic<Option<i64>>>
where
    Device: EnvironmentSensor + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    let requested = if names.is_empty() {
        handle.sensor()?.quantities().iter().collect::<Vec<_>>()
    } else {
        names
            .iter()
            .map(|name| {
                Quantity::from_name(name).ok_or_else(|| {
                    Error::runtime(alloc::format!("unknown environment quantity `{name}`"))
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    ensure_active(&handle.active)?;
    let sensor = handle
        .sensor
        .as_mut()
        .ok_or_else(|| Error::runtime("environment sensor handle is closed"))?;
    let reading = sensor.read().await.map_err(|error| {
        Error::runtime(alloc::format!("environment sensor read failed: {error:?}"))
    })?;
    Ok(values(&reading, &requested))
}

fn values(reading: &EnvironmentReading, quantities: &[Quantity]) -> Variadic<Option<i64>> {
    quantities
        .iter()
        .map(|quantity| reading.value(*quantity))
        .collect::<Vec<_>>()
        .into()
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("environment package has been revoked"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;

    use barracuda_board_hal::environment::Quantities;

    /// An air-quality sensor whose CO2 has no value yet.
    struct TestSensor;

    impl EnvironmentSensor for TestSensor {
        type Error = Infallible;

        fn quantities(&self) -> Quantities {
            Quantities::EMPTY
                .with(Quantity::Temperature)
                .with(Quantity::RelativeHumidity)
                .with(Quantity::Co2)
        }

        async fn read(&mut self) -> core::result::Result<EnvironmentReading, Self::Error> {
            Ok(EnvironmentReading {
                temperature_millicelsius: Some(-1_250),
                relative_humidity_millipercent: Some(45_000),
                ..EnvironmentReading::EMPTY
            })
        }
    }

    fn eval(package: &EnvironmentPackage<TestSensor>, script: &str) -> Result<bool> {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async())
    }

    #[test]
    fn lua_lists_quantities_and_reads_named_values() {
        let package = EnvironmentPackage::new(Some(TestSensor));
        let result = eval(
            &package,
            "local environment = require('environment')\n\
             assert(environment.available())\n\
             local sensor <close> = environment.open()\n\
             local names = { sensor:quantities() }\n\
             local t, rh, co2 = sensor:read()\n\
             local only_rh = sensor:read('relative_humidity')\n\
             local light = sensor:read('illuminance')\n\
             return #names == 3 and names[1] == 'temperature' and names[3] == 'co2'\n\
               and t == -1250 and rh == 45000 and co2 == nil\n\
               and only_rh == 45000 and light == nil and not environment.available()",
        )
        .expect("read sensor");
        assert!(result);
    }

    #[test]
    fn lua_reports_an_unknown_quantity_and_a_closed_handle() {
        let package = EnvironmentPackage::new(Some(TestSensor));
        let result = eval(
            &package,
            "local sensor = require('environment').open()\n\
             local value, unknown = sensor:read('humidity')\n\
             assert(value == nil and unknown:find('unknown environment quantity'), 'unknown name accepted')\n\
             sensor:close()\n\
             local closed_value, closed = sensor:read()\n\
             assert(closed_value == nil and closed:find('closed'), 'closed read accepted')\n\
             return not sensor:is_open()",
        )
        .expect("run script");
        assert!(result);
    }

    #[test]
    fn a_board_without_a_sensor_reports_unavailable() {
        let package = EnvironmentPackage::<TestSensor>::new(None);
        assert!(package.interface().is_none());
        let result = eval(
            &package,
            "local environment = require('environment')\n\
             assert(not environment.available(), 'available')\n\
             local handle, error = environment.open()\n\
             return handle == nil and error:find('unavailable') ~= nil",
        )
        .expect("run script");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = EnvironmentPackage::new(Some(TestSensor));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
