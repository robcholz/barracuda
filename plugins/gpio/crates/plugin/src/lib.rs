//! Lua package for the GPIO values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, sync::Arc};
use barracuda_board_hal::{
    ConfigurableDigitalPin, DigitalLevel, ExposedIo, InputConfig, OutputConfig, OutputDrive, Pull,
    ResourceSet,
};
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_plugin::PLUGIN_ID as VM_PLUGIN_ID;
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal::digital::{InputPin, OutputPin};

/// Stable identity of the GPIO Plugin.
pub const PLUGIN_ID: &str = "gpio";

/// Takes the Board-exposed GPIO value and registers the `gpio` Lua package.
pub struct GpioPlugin<Gpio> {
    hardware: Option<Gpio>,
}

impl<Gpio> GpioPlugin<Gpio> {
    /// Takes exclusive ownership of the GPIO value from shared construction resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo<Gpio = Gpio>,
    {
        Self {
            hardware: context.hal.io.take_gpio(),
        }
    }
}

impl<Gpio, const M: usize> Plugin<M> for GpioPlugin<Gpio>
where
    Gpio: ResourceSet + Send + 'static,
    Gpio::Resource: ConfigurableDigitalPin + Send,
    <Gpio::Resource as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    const DEPENDS_ON: &'static [&'static str] = &[VM_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(VM_PLUGIN_ID)?;
        let registration = registry
            .register(GpioPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedGpio<Gpio> = Arc<Mutex<CriticalSectionRawMutex, Gpio>>;

struct GpioPackage<Gpio> {
    hardware: Option<SharedGpio<Gpio>>,
    active: Arc<AtomicBool>,
}

impl<Gpio> GpioPackage<Gpio> {
    fn new(hardware: Option<Gpio>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Gpio> Package for GpioPackage<Gpio>
where
    Gpio: ResourceSet + Send + 'static,
    Gpio::Resource: ConfigurableDigitalPin + Send,
    <Gpio::Resource as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let available_active = Arc::clone(&self.active);
        let input = self.hardware.clone();
        let input_active = Arc::clone(&self.active);
        let output = self.hardware.clone();
        let output_active = Arc::clone(&self.active);
        let disable = self.hardware.clone();
        let disable_active = Arc::clone(&self.active);
        let read = self.hardware.clone();
        let read_active = Arc::clone(&self.active);
        let write = self.hardware.clone();
        let write_active = Arc::clone(&self.active);

        lua.register_lib("gpio", move |package| {
            package.register_async("available", move |name: String| {
                let hardware = available.clone();
                let active = Arc::clone(&available_active);
                async move {
                    if !active.load(Ordering::Acquire) {
                        return Some(Ok(false));
                    }
                    let exists = match hardware {
                        Some(hardware) => {
                            let hardware = hardware.lock().await;
                            active.load(Ordering::Acquire) && hardware.contains(&name)
                        }
                        None => false,
                    };
                    Some(Ok(exists))
                }
            })?;
            package.register_async("input", move |(name, pull): (String, String)| {
                let hardware = input.clone();
                let active = Arc::clone(&input_active);
                async move {
                    let config = match parse_pull(&pull) {
                        Ok(pull) => InputConfig { pull },
                        Err(error) => return Some(Err(error)),
                    };
                    Some(call_gpio(hardware, active, name, |pin| pin.configure_input(config)).await)
                }
            })?;
            package.register_async(
                "output",
                move |(name, initial, drive): (String, bool, String)| {
                    let hardware = output.clone();
                    let active = Arc::clone(&output_active);
                    async move {
                        let drive = match parse_drive(&drive) {
                            Ok(drive) => drive,
                            Err(error) => return Some(Err(error)),
                        };
                        let config = OutputConfig {
                            initial: if initial {
                                DigitalLevel::High
                            } else {
                                DigitalLevel::Low
                            },
                            drive,
                        };
                        Some(
                            call_gpio(hardware, active, name, |pin| pin.configure_output(config))
                                .await,
                        )
                    }
                },
            )?;
            package.register_async("disable", move |name: String| {
                let hardware = disable.clone();
                let active = Arc::clone(&disable_active);
                async move {
                    Some(call_gpio(hardware, active, name, ConfigurableDigitalPin::disable).await)
                }
            })?;
            package.register_async("read", move |name: String| {
                let hardware = read.clone();
                let active = Arc::clone(&read_active);
                async move { Some(call_gpio(hardware, active, name, InputPin::is_high).await) }
            })?;
            package.register_async("write", move |(name, high): (String, bool)| {
                let hardware = write.clone();
                let active = Arc::clone(&write_active);
                async move {
                    let operation = if high {
                        OutputPin::set_high
                    } else {
                        OutputPin::set_low
                    };
                    Some(call_gpio(hardware, active, name, operation).await)
                }
            })
        })
    }
}

impl<Gpio> LuaPackage for GpioPackage<Gpio>
where
    Gpio: ResourceSet + Send + 'static,
    Gpio::Resource: ConfigurableDigitalPin + Send,
    <Gpio::Resource as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "gpio"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

async fn call_gpio<Gpio, T>(
    hardware: Option<SharedGpio<Gpio>>,
    active: Arc<AtomicBool>,
    name: String,
    operation: impl FnOnce(
        &mut Gpio::Resource,
    ) -> core::result::Result<
        T,
        <Gpio::Resource as embedded_hal::digital::ErrorType>::Error,
    >,
) -> Result<T>
where
    Gpio: ResourceSet,
    Gpio::Resource: ConfigurableDigitalPin,
    <Gpio::Resource as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
{
    let hardware = hardware.ok_or_else(|| Error::runtime("GPIO is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("GPIO package has been revoked"));
    }
    let pin = hardware
        .get_mut(&name)
        .ok_or_else(|| Error::runtime(format!("GPIO `{name}` is not exposed by this Board")))?;
    operation(pin).map_err(|error| Error::runtime(format!("GPIO `{name}` failed: {error:?}")))
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

    use barracuda_board_hal::NamedResources;
    use core::convert::Infallible;
    use embedded_hal::digital::{ErrorType, StatefulOutputPin};
    use futures_lite::future::block_on;

    use super::*;

    struct TestGpio {
        high: bool,
    }

    impl ErrorType for TestGpio {
        type Error = Infallible;
    }

    impl InputPin for TestGpio {
        fn is_high(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(self.high)
        }

        fn is_low(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(!self.high)
        }
    }

    impl OutputPin for TestGpio {
        fn set_low(&mut self) -> core::result::Result<(), Self::Error> {
            self.high = false;
            Ok(())
        }

        fn set_high(&mut self) -> core::result::Result<(), Self::Error> {
            self.high = true;
            Ok(())
        }
    }

    impl StatefulOutputPin for TestGpio {
        fn is_set_high(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(self.high)
        }

        fn is_set_low(&mut self) -> core::result::Result<bool, Self::Error> {
            Ok(!self.high)
        }
    }

    impl ConfigurableDigitalPin for TestGpio {
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
    fn lua_can_change_gpio_mode_and_level() {
        let package = GpioPackage::new(Some(NamedResources::new([(
            "status",
            TestGpio { high: false },
        )])));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");

        let high: bool = block_on(
            lua.load(
                "local gpio = require('gpio')\n\
                 gpio.output('status', true, 'push-pull')\n\
                 gpio.write('status', false)\n\
                 gpio.input('status', 'up')\n\
                 return gpio.available('status') and not gpio.read('status')",
            )
            .eval_async(),
        )
        .expect("run GPIO script");

        assert!(high);
    }

    #[test]
    fn missing_board_gpio_is_reported_to_lua() {
        let package = GpioPackage::<NamedResources<TestGpio, 0>>::new(None);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");
        let unavailable: bool = block_on(
            lua.load(
                "local gpio = require('gpio')\n\
                 local value, err = gpio.read('status')\n\
                 return not gpio.available('status') and value == nil and type(err) == 'string'",
            )
            .eval_async(),
        )
        .expect("run unavailable GPIO script");
        assert!(unavailable);
    }

    #[test]
    fn revocation_disables_callbacks_in_existing_lua_states() {
        let package = GpioPackage::new(Some(NamedResources::new([(
            "status",
            TestGpio { high: false },
        )])));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install GPIO package");
        package.revoke();

        let revoked: bool = block_on(
            lua.load(
                "local gpio = require('gpio')\n\
                 local value, err = gpio.read('status')\n\
                 return not gpio.available('status') and value == nil and type(err) == 'string'",
            )
            .eval_async(),
        )
        .expect("run revoked GPIO script");
        assert!(revoked);
    }
}
