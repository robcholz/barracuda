//! Lua package for the Board's primary built-in IMU.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::string::String;
use barracuda_board_hal::imu::{Imu, ImuDescriptor, ImuPeripheral, ImuSample};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's primary IMU capability and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct ImuPlugin<Device> {
    imu: Option<Device>,
}

impl<Device> ImuPlugin<Device> {
    /// Takes the built-in IMU from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: ImuPeripheral<Imu = Device>,
    {
        Self {
            imu: config::peripherals::INERTIAL_MEASUREMENT_DEVICE
                .and_then(|instance| context.hal.peripherals.take_imu(instance)),
        }
    }
}

impl<Device> Plugin for ImuPlugin<Device>
where
    Device: Imu + Send + 'static,
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
            .register(ImuPackage::new(self.imu.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct ImuPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    imu: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> ImuPackage<Device> {
    fn new(imu: Option<Device>) -> Self {
        Self {
            present: imu.is_some(),
            imu: Arc::new(critical_section::Mutex::new(RefCell::new(imu))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for ImuPackage<Device>
where
    Device: Imu + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_imu = Arc::clone(&self.imu);
        let available_active = Arc::clone(&self.active);
        let open_imu = Arc::clone(&self.imu);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("motion", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_imu.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("imu package has been revoked")));
                }
                let imu =
                    critical_section::with(|section| open_imu.borrow(section).borrow_mut().take());
                Some(match imu {
                    Some(imu) => lua.create_userdata(ImuHandle::new(imu, Arc::clone(&open_active))),
                    None => Err(Error::runtime(
                        "built-in IMU is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for ImuPackage<Device>
where
    Device: Imu + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "motion"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 2,
                purpose: "读取动作传感器",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct ImuHandle<Device> {
    imu: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> ImuHandle<Device> {
    fn new(imu: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            imu: Some(imu),
            active,
        }
    }

    fn close(&mut self) {
        self.imu.take();
    }
}

impl<Device> UserData for ImuHandle<Device>
where
    Device: Imu + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("descriptor", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_async_method("read", |handle, (): ()| async move {
            Some(read_sample(handle).await.map(sample_values))
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.imu.is_some()
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

impl<Device> ImuHandle<Device>
where
    Device: Imu,
{
    fn descriptor(&self) -> Result<ImuDescriptor> {
        ensure_active(&self.active)?;
        self.imu
            .as_ref()
            .map(Imu::descriptor)
            .ok_or_else(|| Error::runtime("imu handle is closed"))
    }
}

impl<Device> Drop for ImuHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

async fn read_sample<Device>(handle: UserDataHandle<ImuHandle<Device>>) -> Result<ImuSample>
where
    Device: Imu + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let imu = handle
        .imu
        .as_mut()
        .ok_or_else(|| Error::runtime("imu handle is closed"))?;
    imu.read_sample()
        .await
        .map_err(|error| Error::runtime(alloc::format!("IMU read failed: {error:?}")))
}

type DescriptorValues = (i64, i64, i64, i64, bool, Option<i64>);

fn descriptor_values(descriptor: ImuDescriptor) -> DescriptorValues {
    (
        i64::from(descriptor.accelerometer_rate_hz()),
        i64::from(descriptor.gyroscope_rate_hz()),
        i64::from(descriptor.accelerometer_range_mg()),
        i64::from(descriptor.gyroscope_range_mdps()),
        descriptor.has_temperature(),
        descriptor.magnetometer_rate_hz().map(i64::from),
    )
}

type SampleValues = (
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn sample_values(sample: ImuSample) -> SampleValues {
    let acceleration = sample.acceleration_mg();
    let angular_velocity = sample.angular_velocity_mdps();
    let field = sample.magnetic_field_nt();
    (
        i64::from(acceleration.x),
        i64::from(acceleration.y),
        i64::from(acceleration.z),
        i64::from(angular_velocity.x),
        i64::from(angular_velocity.y),
        i64::from(angular_velocity.z),
        sample.temperature_mc().map(i64::from),
        field.map(|field| i64::from(field.x)),
        field.map(|field| i64::from(field.y)),
        field.map(|field| i64::from(field.z)),
    )
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("imu package has been revoked"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;

    use barracuda_board_hal::imu::{ImuDescriptor, ImuSample, Vector3};

    struct TestImu;

    impl Imu for TestImu {
        type Error = Infallible;

        fn descriptor(&self) -> ImuDescriptor {
            ImuDescriptor::new(100, 100, 4_000, 500_000, true).with_magnetometer(25)
        }

        async fn read_sample(&mut self) -> core::result::Result<ImuSample, Self::Error> {
            Ok(
                ImuSample::new(Vector3::new(1, 2, 3), Vector3::new(4, 5, 6), Some(25_000))
                    .with_magnetic_field(Vector3::new(20_000, -5_000, 40_000)),
            )
        }
    }

    #[test]
    fn lua_reads_descriptor_and_sample() {
        let package = ImuPackage::new(Some(TestImu));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local imus = require('motion')\nlocal device <close> = imus.open()\nlocal ar, gr, afs, gfs, temp, mr = device:descriptor()\nlocal ax, ay, az, gx, gy, gz, t, mx, my, mz = device:read()\nreturn ar == 100 and gr == 100 and afs == 4000 and gfs == 500000 and temp and mr == 25 and ax == 1 and ay == 2 and az == 3 and gx == 4 and gy == 5 and gz == 6 and t == 25000 and mx == 20000 and my == -5000 and mz == 40000")
                .eval_async(),
        )
        .expect("read IMU sample");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = ImuPackage::new(Some(TestImu));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
