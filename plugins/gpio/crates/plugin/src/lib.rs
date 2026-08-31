//! Lua package for the GPIO resources explicitly exposed by the selected Board.

#![no_std]

extern crate alloc;

use alloc::{string::String, sync::Arc};
use barracuda_board_hal::{
    DigitalLevel, GpioService, InputConfig, OutputConfig, OutputDrive, Pull,
};
use barracuda_lua::{Error, Lua, Package, Result};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_plugin::PLUGIN_ID as VM_PLUGIN_ID;

/// Stable identity of the GPIO Plugin.
pub const PLUGIN_ID: &str = "gpio";

/// Registers the `gpio` Lua package with the VM Plugin.
pub struct GpioPlugin {
    service: Option<Arc<dyn GpioService>>,
}

impl GpioPlugin {
    /// Creates the Plugin from Board hardware resources.
    #[must_use]
    pub fn new(context: &PluginContext) -> Self {
        Self {
            service: context.hardware_services.gpio(),
        }
    }
}

impl<const M: usize> Plugin<M> for GpioPlugin {
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
            .register(GpioPackage {
                service: self.service.clone(),
            })
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct GpioPackage {
    service: Option<Arc<dyn GpioService>>,
}

impl Package for GpioPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = self.service.clone();
        let input = self.service.clone();
        let output = self.service.clone();
        let disable = self.service.clone();
        let read = self.service.clone();
        let write = self.service.clone();

        lua.register_lib("gpio", move |package| {
            package.register("available", move |name: String| {
                Some(Ok(available
                    .as_ref()
                    .is_some_and(|service| service.contains(&name))))
            })?;
            package.register_async("input", move |(name, pull): (String, String)| {
                let service = input.clone();
                async move {
                    let config = match parse_pull(&pull) {
                        Ok(pull) => InputConfig { pull },
                        Err(error) => return Some(Err(error)),
                    };
                    Some(call_gpio(service, |service| service.configure_input(name, config)).await)
                }
            })?;
            package.register_async(
                "output",
                move |(name, initial, drive): (String, bool, String)| {
                    let service = output.clone();
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
                            call_gpio(service, |service| service.configure_output(name, config))
                                .await,
                        )
                    }
                },
            )?;
            package.register_async("disable", move |name: String| {
                let service = disable.clone();
                async move { Some(call_gpio(service, |service| service.disable(name)).await) }
            })?;
            package.register_async("read", move |name: String| {
                let service = read.clone();
                async move { Some(call_gpio(service, |service| service.read(name)).await) }
            })?;
            package.register_async("write", move |(name, high): (String, bool)| {
                let service = write.clone();
                async move { Some(call_gpio(service, |service| service.write(name, high)).await) }
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
    service: Option<Arc<dyn GpioService>>,
    operation: impl for<'a> FnOnce(&'a dyn GpioService) -> barracuda_board_hal::ServiceFuture<'a, T>,
) -> Result<T> {
    let service = service.ok_or_else(|| Error::runtime("GPIO is not exposed by this Board"))?;
    operation(service.as_ref())
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

    use alloc::{boxed::Box, sync::Arc};
    use barracuda_board_hal::{IoServiceResult, ServiceFuture};
    use futures_lite::future::block_on;
    use std::sync::Mutex;

    use super::*;

    struct TestGpio {
        high: Mutex<bool>,
    }

    impl GpioService for TestGpio {
        fn contains(&self, name: &str) -> bool {
            name == "status"
        }

        fn configure_input(&self, _name: String, _config: InputConfig) -> ServiceFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn configure_output(&self, _name: String, config: OutputConfig) -> ServiceFuture<'_, ()> {
            Box::pin(async move {
                *self.high.lock().expect("lock GPIO") = config.initial == DigitalLevel::High;
                Ok(())
            })
        }

        fn disable(&self, _name: String) -> ServiceFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn read(&self, _name: String) -> ServiceFuture<'_, bool> {
            Box::pin(async move { Ok(*self.high.lock().expect("lock GPIO")) })
        }

        fn write(&self, _name: String, high: bool) -> ServiceFuture<'_, ()> {
            Box::pin(async move {
                *self.high.lock().expect("lock GPIO") = high;
                Ok(())
            })
        }
    }

    #[test]
    fn lua_can_change_gpio_mode_and_level() -> IoServiceResult<()> {
        let package = GpioPackage {
            service: Some(Arc::new(TestGpio {
                high: Mutex::new(false),
            })),
        };
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
        let package = GpioPackage { service: None };
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
