//! Lua package for runtime digital handles over Board-exposed pins.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, string::ToString};
use barracuda_board_hal::{
    ConfigurableDigitalPin, DigitalLevel, DigitalProvider, ExposedIo, InputConfig, OutputConfig,
    OutputDrive, Pull,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Shares the unified exposed-I/O owner with the `gpio` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct GpioPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> GpioPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + DigitalProvider,
    {
        Self {
            io: Arc::clone(&context.hal.exposed_io),
        }
    }
}

impl<Io> Plugin for GpioPlugin<Io>
where
    Io: ExposedIo + DigitalProvider,
    Io::Pin: Send,
    Io::Error: core::fmt::Display,
    <Io::Pin as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
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
            .register(GpioPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct GpioPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> GpioPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for GpioPackage<Io>
where
    Io: DigitalProvider + Send + Sync + 'static,
    Io::Pin: Send,
    Io::Error: core::fmt::Display,
    <Io::Pin as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = Arc::clone(&self.io);
        let available_active = Arc::clone(&self.active);
        let input = Arc::clone(&self.io);
        let input_active = Arc::clone(&self.active);
        let output = Arc::clone(&self.io);
        let output_active = Arc::clone(&self.active);

        lua.register_lib("gpio", move |package| {
            package.register("available", move |name: String| {
                Some(Ok(
                    available_active.load(Ordering::Acquire) && available.digital_available(&name)
                ))
            })?;
            package.register_with("open_input", move |lua, (name, pull): (String, String)| {
                if !input_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("GPIO package has been revoked")));
                }
                let result = (|| {
                    let mut pin = input
                        .acquire_digital(&name)
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    pin.configure_input(InputConfig {
                        pull: parse_pull(&pull)?,
                    })
                    .map_err(|error| gpio_error(&name, error))?;
                    lua.create_userdata(GpioHandle::new(name, pin, Arc::clone(&input_active)))
                })();
                Some(result)
            })?;
            package.register_with(
                "open_output",
                move |lua, (name, initial, drive): (String, bool, String)| {
                    if !output_active.load(Ordering::Acquire) {
                        return Some(Err(Error::runtime("GPIO package has been revoked")));
                    }
                    let result = (|| {
                        let mut pin = output
                            .acquire_digital(&name)
                            .map_err(|error| Error::runtime(error.to_string()))?;
                        pin.configure_output(OutputConfig {
                            initial: if initial {
                                DigitalLevel::High
                            } else {
                                DigitalLevel::Low
                            },
                            drive: parse_drive(&drive)?,
                        })
                        .map_err(|error| gpio_error(&name, error))?;
                        lua.create_userdata(GpioHandle::new(name, pin, Arc::clone(&output_active)))
                    })();
                    Some(result)
                },
            )
        })
    }
}

impl<Io> LuaPackage for GpioPackage<Io>
where
    Io: DigitalProvider + Send + Sync + 'static,
    Io::Pin: Send,
    Io::Error: core::fmt::Display,
    <Io::Pin as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "gpio"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct GpioHandle<Pin>
where
    Pin: ConfigurableDigitalPin,
{
    name: String,
    pin: Option<Pin>,
    active: Arc<AtomicBool>,
}

impl<Pin> GpioHandle<Pin>
where
    Pin: ConfigurableDigitalPin,
{
    fn new(name: String, pin: Pin, active: Arc<AtomicBool>) -> Self {
        Self {
            name,
            pin: Some(pin),
            active,
        }
    }

    fn pin(&self) -> Result<&Pin> {
        ensure_active(&self.active)?;
        self.pin
            .as_ref()
            .ok_or_else(|| Error::runtime("GPIO handle is closed"))
    }

    fn pin_mut(&mut self) -> Result<&mut Pin> {
        ensure_active(&self.active)?;
        self.pin
            .as_mut()
            .ok_or_else(|| Error::runtime("GPIO handle is closed"))
    }

    fn close(&mut self) {
        if let Some(mut pin) = self.pin.take() {
            let _ = pin.disable();
        }
    }
}

impl<Pin> UserData for GpioHandle<Pin>
where
    Pin: ConfigurableDigitalPin + Send + 'static,
    <Pin as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method_mut("read", |handle, (): ()| {
            let name = handle.name.clone();
            let result = (|| {
                handle
                    .pin_mut()?
                    .is_high()
                    .map_err(|error| gpio_error(&name, error))
            })();
            Some(result)
        });
        methods.add_method_mut("write", |handle, high: bool| {
            let name = handle.name.clone();
            let result = (|| {
                let result = if high {
                    handle.pin_mut()?.set_high()
                } else {
                    handle.pin_mut()?.set_low()
                };
                result.map_err(|error| gpio_error(&name, error))
            })();
            Some(result)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.pin().is_ok()
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

impl<Pin> Drop for GpioHandle<Pin>
where
    Pin: ConfigurableDigitalPin,
{
    fn drop(&mut self) {
        self.close();
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("GPIO package has been revoked"))
    }
}

fn gpio_error(name: &str, error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("GPIO `{name}` failed: {error:?}"))
}

fn parse_pull(value: &str) -> Result<Pull> {
    match value {
        "none" => Ok(Pull::None),
        "up" => Ok(Pull::Up),
        "down" => Ok(Pull::Down),
        _ => Err(Error::runtime("pull must be `none`, `up`, or `down`")),
    }
}

fn parse_drive(value: &str) -> Result<OutputDrive> {
    match value {
        "push-pull" => Ok(OutputDrive::PushPull),
        "open-drain" => Ok(OutputDrive::OpenDrain),
        _ => Err(Error::runtime("drive must be `push-pull` or `open-drain`")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};
    use portable_atomic::AtomicBool;

    use super::*;

    struct TestProvider {
        busy: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct TestOpenError;

    impl core::fmt::Display for TestOpenError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("pin is busy or absent")
        }
    }

    impl core::error::Error for TestOpenError {}

    struct TestPin {
        high: bool,
        busy: Arc<AtomicBool>,
    }

    impl Drop for TestPin {
        fn drop(&mut self) {
            self.busy.store(false, Ordering::Release);
        }
    }

    impl DigitalProvider for TestProvider {
        type Pin = TestPin;
        type Error = TestOpenError;

        fn digital_available(&self, name: &str) -> bool {
            name == "D1"
        }

        fn acquire_digital(&self, name: &str) -> core::result::Result<Self::Pin, Self::Error> {
            if name != "D1" || self.busy.swap(true, Ordering::AcqRel) {
                return Err(TestOpenError);
            }
            Ok(TestPin {
                high: false,
                busy: Arc::clone(&self.busy),
            })
        }
    }

    impl ErrorType for TestPin {
        type Error = Infallible;
    }

    impl InputPin for TestPin {
        fn is_high(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(self.high)
        }

        fn is_low(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(!self.high)
        }
    }

    impl OutputPin for TestPin {
        fn set_low(&mut self) -> core::result::Result<(), Self::Error> {
            self.high = false;
            Ok(())
        }

        fn set_high(&mut self) -> core::result::Result<(), Self::Error> {
            self.high = true;
            Ok(())
        }
    }

    impl StatefulOutputPin for TestPin {
        fn is_set_high(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(self.high)
        }

        fn is_set_low(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(!self.high)
        }
    }

    impl ConfigurableDigitalPin for TestPin {
        fn configure_input(
            &mut self,
            _config: InputConfig,
        ) -> core::result::Result<(), Self::Error> {
            Ok(())
        }

        fn configure_output(
            &mut self,
            config: OutputConfig,
        ) -> core::result::Result<(), Self::Error> {
            self.high = config.initial == DigitalLevel::High;
            Ok(())
        }

        fn disable(&mut self) -> core::result::Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn lua_handles_own_and_drop_provider_values() {
        let provider = Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        });
        let package = GpioPackage::new(Arc::clone(&provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");

        let result: bool = lua
            .load(
                "local gpio = require('gpio')\n\
                 local pin <close> = gpio.open_output('D1', false, 'push-pull')\n\
                 pin:write(true)\n\
                 return gpio.available('D1') and pin:read() and pin:is_open()",
            )
            .eval()
            .expect("run GPIO application");

        assert!(result);
        assert!(!provider.busy.load(Ordering::Acquire));
    }

    #[test]
    fn a_second_handle_observes_the_provider_conflict() {
        let package = GpioPackage::new(Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");

        let busy: bool = lua
            .load(
                "local gpio = require('gpio')\n\
                 local first <close> = gpio.open_input('D1', 'up')\n\
                 local second, err = gpio.open_output('D1', false, 'push-pull')\n\
                 return second == nil and type(err) == 'string'",
            )
            .eval()
            .expect("run conflicting application");

        assert!(busy);
    }

    #[test]
    fn revocation_rejects_new_handles() {
        let package = GpioPackage::new(Arc::new(TestProvider {
            busy: Arc::new(AtomicBool::new(false)),
        }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");
        package.revoke();

        let revoked: bool = lua
            .load(
                "local gpio = require('gpio')\n\
                 local pin, err = gpio.open_input('D1', 'none')\n\
                 return not gpio.available('D1') and pin == nil and type(err) == 'string'",
            )
            .eval()
            .expect("run revoked application");
        assert!(revoked);
    }
}
