//! Lua package for the GPIO values explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc};
use barracuda_board_hal::{DigitalLevel, InputConfig, OutputConfig, OutputDrive, Pull};
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::{LuaGpioHardware, LuaHardwareFuture, LuaIo, PluginContext};
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

/// Takes the Board-exposed GPIO value and registers the `gpio` Lua package.
pub struct GpioPlugin {
    hardware: Option<Box<dyn LuaGpioHardware>>,
}

impl GpioPlugin {
    /// Takes exclusive ownership of the GPIO value from shared construction resources.
    #[must_use]
    pub fn new<Builtins, Io: LuaIo>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            hardware: context.hal.io.take_gpio(),
        }
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for GpioPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(<Self as Plugin<M>>::DEPENDS_ON[0])?;
        let registration = registry
            .register(GpioPackage::new(self.hardware.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

type SharedGpio = Arc<Mutex<CriticalSectionRawMutex, Box<dyn LuaGpioHardware>>>;

struct GpioPackage {
    hardware: Option<SharedGpio>,
}

impl GpioPackage {
    fn new(hardware: Option<Box<dyn LuaGpioHardware>>) -> Self {
        Self {
            hardware: hardware.map(|hardware| Arc::new(Mutex::new(hardware))),
        }
    }
}

impl Package for GpioPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.hardware.clone();
        let input = self.hardware.clone();
        let output = self.hardware.clone();
        let disable = self.hardware.clone();
        let read = self.hardware.clone();
        let write = self.hardware.clone();

        lua.register_lib("gpio", move |package| {
            package.register_async("available", move |name: String| {
                let hardware = available.clone();
                async move {
                    let exists = match hardware {
                        Some(hardware) => hardware.lock().await.contains(&name),
                        None => false,
                    };
                    Some(Ok(exists))
                }
            })?;
            package.register_async("input", move |(name, pull): (String, String)| {
                let hardware = input.clone();
                async move {
                    let config = match parse_pull(&pull) {
                        Ok(pull) => InputConfig { pull },
                        Err(error) => return Some(Err(error)),
                    };
                    Some(
                        call_gpio(hardware, |hardware| hardware.configure_input(name, config))
                            .await,
                    )
                }
            })?;
            package.register_async(
                "output",
                move |(name, initial, drive): (String, bool, String)| {
                    let hardware = output.clone();
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
                            call_gpio(hardware, |hardware| hardware.configure_output(name, config))
                                .await,
                        )
                    }
                },
            )?;
            package.register_async("disable", move |name: String| {
                let hardware = disable.clone();
                async move { Some(call_gpio(hardware, |hardware| hardware.disable(name)).await) }
            })?;
            package.register_async("read", move |name: String| {
                let hardware = read.clone();
                async move { Some(call_gpio(hardware, |hardware| hardware.read(name)).await) }
            })?;
            package.register_async("write", move |(name, high): (String, bool)| {
                let hardware = write.clone();
                async move {
                    Some(call_gpio(hardware, |hardware| hardware.write(name, high)).await)
                }
            })
        })
    }
}

impl LuaPackage for GpioPackage {
    fn name(&self) -> &'static str {
        "gpio"
    }
}

async fn call_gpio<T>(
    hardware: Option<SharedGpio>,
    operation: impl for<'a> FnOnce(&'a mut dyn LuaGpioHardware) -> LuaHardwareFuture<'a, T>,
) -> Result<T> {
    let hardware = hardware.ok_or_else(|| Error::runtime("GPIO is not exposed by this Board"))?;
    let mut hardware = hardware.lock().await;
    operation(hardware.as_mut())
        .await
        .map_err(|error| Error::runtime(error.message()))
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

    use alloc::boxed::Box;
    use barracuda_plugin_api::{LuaHardwareFuture, LuaHardwareResult};
    use futures_lite::future::block_on;
    use std::sync::Mutex as StdMutex;

    use super::*;

    struct TestGpio {
        high: StdMutex<bool>,
    }

    impl LuaGpioHardware for TestGpio {
        fn contains(&self, name: &str) -> bool {
            name == "status"
        }

        fn configure_input(
            &mut self,
            _name: String,
            _config: InputConfig,
        ) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn configure_output(
            &mut self,
            _name: String,
            config: OutputConfig,
        ) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async move {
                *self.high.lock().expect("lock GPIO") = config.initial == DigitalLevel::High;
                Ok(())
            })
        }

        fn disable(&mut self, _name: String) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn read(&mut self, _name: String) -> LuaHardwareFuture<'_, bool> {
            Box::pin(async move { Ok(*self.high.lock().expect("lock GPIO")) })
        }

        fn write(&mut self, _name: String, high: bool) -> LuaHardwareFuture<'_, ()> {
            Box::pin(async move {
                *self.high.lock().expect("lock GPIO") = high;
                Ok(())
            })
        }
    }

    #[test]
    fn lua_can_change_gpio_mode_and_level() -> LuaHardwareResult<()> {
        let package = GpioPackage::new(Some(Box::new(TestGpio {
            high: StdMutex::new(false),
        })));
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
        Ok(())
    }

    #[test]
    fn missing_board_gpio_is_reported_to_lua() {
        let package = GpioPackage::new(None);
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
}
