//! Lua package for the Board's primary built-in IMU.

#![no_std]

extern crate alloc;

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
            imu: context.hal.peripherals.take_imu(),
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
    imu: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> ImuPackage<Device> {
    fn new(imu: Option<Device>) -> Self {
        Self {
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
        lua.register_lib("imu", move |package| {
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
        "imu"
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

fn descriptor_values(descriptor: ImuDescriptor) -> (i64, i64, i64, i64, bool) {
    (
        i64::from(descriptor.accelerometer_rate_hz()),
        i64::from(descriptor.gyroscope_rate_hz()),
        i64::from(descriptor.accelerometer_range_mg()),
        i64::from(descriptor.gyroscope_range_mdps()),
        descriptor.has_temperature(),
    )
}

fn sample_values(sample: ImuSample) -> (i64, i64, i64, i64, i64, i64, Option<i64>) {
    let acceleration = sample.acceleration_mg();
    let angular_velocity = sample.angular_velocity_mdps();
    (
        i64::from(acceleration.x),
        i64::from(acceleration.y),
        i64::from(acceleration.z),
        i64::from(angular_velocity.x),
        i64::from(angular_velocity.y),
        i64::from(angular_velocity.z),
        sample.temperature_mc().map(i64::from),
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
            ImuDescriptor::new(100, 100, 4_000, 500_000, true)
        }

        async fn read_sample(&mut self) -> core::result::Result<ImuSample, Self::Error> {
            Ok(ImuSample::new(
                Vector3::new(1, 2, 3),
                Vector3::new(4, 5, 6),
                Some(25_000),
            ))
        }
    }

    #[test]
    fn lua_reads_descriptor_and_sample() {
        let package = ImuPackage::new(Some(TestImu));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local imus = require('imu')\nlocal device <close> = imus.open()\nlocal ar, gr, afs, gfs, temp = device:descriptor()\nlocal ax, ay, az, gx, gy, gz, t = device:read()\nreturn ar == 100 and gr == 100 and afs == 4000 and gfs == 500000 and temp and ax == 1 and ay == 2 and az == 3 and gx == 4 and gy == 5 and gz == 6 and t == 25000")
                .eval_async(),
        )
        .expect("read IMU sample");
        assert!(result);
    }
}
