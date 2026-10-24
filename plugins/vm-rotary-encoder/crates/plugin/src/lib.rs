//! Lua package for the Board's built-in rotary encoders.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{format, string::String, vec::Vec};
use barracuda_board_hal::rotary_encoder::{RotaryEncoder, RotaryEncoderPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result,
    UserData, UserDataMethods, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's rotary encoders and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct RotaryEncoderPlugin<Device> {
    encoders: Vec<(&'static str, Device)>,
}

impl<Device> RotaryEncoderPlugin<Device> {
    /// Takes the built-in rotary encoders from the generated Board capability
    /// set, the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: RotaryEncoderPeripheral<RotaryEncoder = Device>,
    {
        Self {
            encoders: config::peripherals::ROTARY_ENCODER_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_rotary_encoder(*instance)
                        .map(|encoder| (*name, encoder))
                })
                .collect(),
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
            .register(EncoderPackage::new(core::mem::take(&mut self.encoders)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct EncoderPackage<Device> {
    encoders: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> EncoderPackage<Device> {
    fn new(encoders: Vec<(&'static str, Device)>) -> Self {
        Self {
            encoders: BoardDevices::new(encoders),
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
        let available = (self.encoders.clone(), Arc::clone(&self.active));
        let list = self.encoders.clone();
        let open = (self.encoders.clone(), Arc::clone(&self.active));
        lua.register_lib("encoder", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (encoders, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && encoders.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_with("open", move |lua, name: Variadic<String>| {
                let (encoders, active) = &open;
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("encoder package has been revoked")));
                }
                let name = name.first().map(String::as_str);
                Some(match encoders.take(name) {
                    Some(encoder) => {
                        lua.create_userdata(EncoderHandle::new(encoder, Arc::clone(active)))
                    }
                    None => Err(Error::runtime(match name {
                        Some(name) => {
                            format!("rotary encoder `{name}` is unavailable or already open")
                        }
                        None => {
                            String::from("built-in rotary encoder is unavailable or already open")
                        }
                    })),
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
        (!self.encoders.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "Read the knob",
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
        let package = EncoderPackage::new(std::vec![("knob", encoder())]);
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
    fn lua_lists_and_opens_each_encoder_the_primary_by_default() {
        let package = EncoderPackage::new(std::vec![("knob", encoder()), ("side", encoder())]);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local encoder = require('encoder')\nlocal names = encoder.list()\nlocal side <close> = encoder.open('side')\nlocal knob <close> = encoder.open()\nlocal again, problem = encoder.open('side')\nreturn #names == 2 and names[1] == 'knob' and names[2] == 'side' and side:position() == 2 and knob:is_open() and again == nil and problem ~= nil and not encoder.available() and not encoder.available('missing')")
                .eval_async(),
        )
        .expect("open encoders");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = EncoderPackage::new(std::vec![("knob", encoder())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
