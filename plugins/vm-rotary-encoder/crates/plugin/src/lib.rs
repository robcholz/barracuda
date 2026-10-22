//! Lua package for the Board's primary built-in rotary encoder.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{format, string::String};
use barracuda_board_hal::rotary_encoder::{RotaryEncoder, RotaryEncoderPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's primary rotary encoder and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct RotaryEncoderPlugin<Device> {
    encoder: Option<Device>,
}

impl<Device> RotaryEncoderPlugin<Device> {
    /// Takes the built-in rotary encoder from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: RotaryEncoderPeripheral<RotaryEncoder = Device>,
    {
        Self {
            encoder: config::peripherals::ROTARY_ENCODER_DEVICE
                .and_then(|instance| context.hal.peripherals.take_rotary_encoder(instance)),
        }
    }
}

impl<Device> Plugin for RotaryEncoderPlugin<Device>
where
    Device: RotaryEncoder + Send + 'static,
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
            .register(EncoderPackage::new(self.encoder.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct EncoderPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    encoder: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> EncoderPackage<Device> {
    fn new(encoder: Option<Device>) -> Self {
        Self {
            present: encoder.is_some(),
            encoder: Arc::new(critical_section::Mutex::new(RefCell::new(encoder))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for EncoderPackage<Device>
where
    Device: RotaryEncoder + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_encoder = Arc::clone(&self.encoder);
        let available_active = Arc::clone(&self.active);
        let open_encoder = Arc::clone(&self.encoder);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("encoder", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_encoder.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("encoder package has been revoked")));
                }
                let encoder = critical_section::with(|section| {
                    open_encoder.borrow(section).borrow_mut().take()
                });
                Some(match encoder {
                    Some(encoder) => {
                        lua.create_userdata(EncoderHandle::new(encoder, Arc::clone(&open_active)))
                    }
                    None => Err(Error::runtime(
                        "built-in rotary encoder is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for EncoderPackage<Device>
where
    Device: RotaryEncoder + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "encoder"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "读取旋钮转动",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct EncoderHandle<Device> {
    encoder: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> EncoderHandle<Device> {
    fn new(encoder: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            encoder: Some(encoder),
            active,
        }
    }

    fn close(&mut self) {
        self.encoder.take();
    }
}

impl<Device> EncoderHandle<Device>
where
    Device: RotaryEncoder,
    Device::Error: core::fmt::Debug,
{
    fn with<T>(
        &mut self,
        operation: impl FnOnce(&mut Device) -> core::result::Result<T, Device::Error>,
    ) -> Result<T> {
        if !self.active.load(Ordering::Acquire) {
            return Err(Error::runtime("encoder package has been revoked"));
        }
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| Error::runtime("encoder handle is closed"))?;
        operation(encoder)
            .map_err(|error| Error::runtime(format!("rotary encoder failed: {error:?}")))
    }
}

impl<Device> UserData for EncoderHandle<Device>
where
    Device: RotaryEncoder + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method_mut("position", |handle, (): ()| {
            Some(handle.with(RotaryEncoder::position))
        });
        methods.add_method_mut("delta", |handle, (): ()| {
            Some(handle.with(RotaryEncoder::delta))
        });
        methods.add_method_mut("reset", |handle, (): ()| {
            handle.with(RotaryEncoder::reset).err().map(Err::<(), _>)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.encoder.is_some()
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

impl<Device> Drop for EncoderHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;

    /// Turns two edges forward on every read.
    struct TestEncoder {
        position: i64,
        mark: i64,
    }

    impl RotaryEncoder for TestEncoder {
        type Error = Infallible;

        fn position(&mut self) -> core::result::Result<i64, Self::Error> {
            self.position += 2;
            Ok(self.position)
        }

        fn delta(&mut self) -> core::result::Result<i64, Self::Error> {
            let position = self.position()?;
            let delta = position - self.mark;
            self.mark = position;
            Ok(delta)
        }

        fn reset(&mut self) -> core::result::Result<(), Self::Error> {
            self.position = 0;
            self.mark = 0;
            Ok(())
        }
    }

    fn encoder() -> TestEncoder {
        TestEncoder {
            position: 0,
            mark: 0,
        }
    }

    #[test]
    fn lua_reads_position_and_delta() {
        let package = EncoderPackage::new(Some(encoder()));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local encoder = require('encoder')\nlocal knob <close> = encoder.open()\nlocal p = knob:position()\nlocal d = knob:delta()\nknob:reset()\nlocal q = knob:position()\nreturn p == 2 and d == 4 and q == 2 and knob:is_open()")
                .eval_async(),
        )
        .expect("read encoder");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = EncoderPackage::new(Some(encoder()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
