//! Lua package for PWM outputs on Board-exposed pins.

#![no_std]

extern crate alloc;

use alloc::{string::String, string::ToString};
use barracuda_board_hal::{ExposedIo, PwmProvider, PwmRequest};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use embedded_hal::pwm::SetDutyCycle;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Shares the unified exposed-I/O owner with the `pwm` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct PwmPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> PwmPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + PwmProvider,
    {
        Self {
            io: Arc::clone(&context.hal.exposed_io),
        }
    }
}

impl<Io> Plugin for PwmPlugin<Io>
where
    Io: ExposedIo + PwmProvider,
    Io::Error: core::fmt::Display,
    <Io::Output as embedded_hal::pwm::ErrorType>::Error: core::fmt::Debug,
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
            .register(PwmPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct PwmPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}
impl<Io> PwmPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for PwmPackage<Io>
where
    Io: PwmProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Output as embedded_hal::pwm::ErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = Arc::clone(&self.io);
        let available_active = Arc::clone(&self.active);
        let io = Arc::clone(&self.io);
        let active = Arc::clone(&self.active);
        lua.register_lib("pwm", move |package| {
            package.register("available", move |name: String| {
                Some(Ok(
                    available_active.load(Ordering::Acquire) && available.pwm_available(&name)
                ))
            })?;
            package.register_with("open", move |lua, (name, frequency_hz): (String, i64)| {
                Some((|| {
                    ensure_active(&active)?;
                    let frequency_hz = positive_u32(frequency_hz)?;
                    let output = io
                        .open_pwm(PwmRequest {
                            pin: &name,
                            frequency_hz,
                        })
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    lua.create_userdata(PwmHandle::new(output, Arc::clone(&active)))
                })())
            })
        })
    }
}

impl<Io> LuaPackage for PwmPackage<Io>
where
    Io: PwmProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Output as embedded_hal::pwm::ErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "pwm"
    }
    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct PwmHandle<Output> {
    output: Option<Output>,
    active: Arc<AtomicBool>,
}
impl<Output> PwmHandle<Output> {
    fn new(output: Output, active: Arc<AtomicBool>) -> Self {
        Self {
            output: Some(output),
            active,
        }
    }
    fn close(&mut self) {
        self.output.take();
    }
}

impl<Output> UserData for PwmHandle<Output>
where
    Output: SetDutyCycle + 'static,
    Output::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("max_duty", |handle, (): ()| {
            Some(
                handle
                    .output()
                    .map(|output| i64::from(output.max_duty_cycle())),
            )
        });
        methods.add_method_mut("set_duty", |handle, duty: i64| {
            Some((|| {
                let duty = u16::try_from(duty)
                    .map_err(|_| Error::runtime("PWM duty must be between 0 and 65535"))?;
                if duty > handle.output()?.max_duty_cycle() {
                    return Err(Error::runtime("PWM duty exceeds max_duty"));
                }
                handle.output_mut()?.set_duty_cycle(duty).map_err(pwm_error)
            })())
        });
        methods.add_method_mut("set_percent", |handle, percent: i64| {
            Some((|| {
                let percent = u8::try_from(percent)
                    .map_err(|_| Error::runtime("PWM percent must be between 0 and 100"))?;
                if percent > 100 {
                    return Err(Error::runtime("PWM percent must be between 0 and 100"));
                }
                handle
                    .output_mut()?
                    .set_duty_cycle_percent(percent)
                    .map_err(pwm_error)
            })())
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.output.is_some()
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

impl<Output: SetDutyCycle> PwmHandle<Output> {
    fn output(&self) -> Result<&Output> {
        ensure_active(&self.active)?;
        self.output
            .as_ref()
            .ok_or_else(|| Error::runtime("PWM handle is closed"))
    }
    fn output_mut(&mut self) -> Result<&mut Output> {
        ensure_active(&self.active)?;
        self.output
            .as_mut()
            .ok_or_else(|| Error::runtime("PWM handle is closed"))
    }
}
impl<Output> Drop for PwmHandle<Output> {
    fn drop(&mut self) {
        self.close();
    }
}

fn positive_u32(value: i64) -> Result<u32> {
    let value = u32::try_from(value)
        .map_err(|_| Error::runtime("PWM frequency must be a positive 32-bit integer"))?;
    if value == 0 {
        Err(Error::runtime("PWM frequency must be greater than zero"))
    } else {
        Ok(value)
    }
}
fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("PWM package has been revoked"))
    }
}
fn pwm_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("PWM operation failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    extern crate std;
    use super::*;
    use core::convert::Infallible;
    struct Output(u16);
    impl embedded_hal::pwm::ErrorType for Output {
        type Error = Infallible;
    }
    impl SetDutyCycle for Output {
        fn max_duty_cycle(&self) -> u16 {
            1000
        }
        fn set_duty_cycle(&mut self, duty: u16) -> core::result::Result<(), Self::Error> {
            self.0 = duty;
            Ok(())
        }
    }
    struct Provider;
    #[derive(Debug)]
    struct OpenError;
    impl core::fmt::Display for OpenError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("unavailable")
        }
    }
    impl core::error::Error for OpenError {}
    impl PwmProvider for Provider {
        type Output = Output;
        type Error = OpenError;
        fn pwm_available(&self, name: &str) -> bool {
            name == "D1"
        }
        fn open_pwm(
            &self,
            request: PwmRequest<'_>,
        ) -> core::result::Result<Self::Output, Self::Error> {
            if request.pin == "D1" && request.frequency_hz == 1000 {
                Ok(Output(0))
            } else {
                Err(OpenError)
            }
        }
    }
    #[test]
    fn lua_configures_pwm_through_embedded_hal() {
        let package = PwmPackage::new(Arc::new(Provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        let result: bool = lua.load("local pwm = require('pwm')\nlocal output <close> = pwm.open('D1', 1000)\noutput:set_percent(25)\nreturn output:max_duty() == 1000 and output:is_open()").eval().expect("use PWM");
        assert!(result);
    }
}
