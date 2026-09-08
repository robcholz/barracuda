//! Lua package for analog functions on Board-exposed pins.

#![no_std]

extern crate alloc;

use alloc::{
    string::{String, ToString},
    sync::Arc,
};
use barracuda_board_hal::{AnalogInput, AnalogOutput, AnalogProvider, ExposedIo};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use core::sync::atomic::{AtomicBool, Ordering};

/// Shares the unified exposed-I/O owner with the `analog` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct AnalogPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> AnalogPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + AnalogProvider,
    {
        Self {
            io: Arc::clone(&context.hal.io),
        }
    }
}

impl<Io> Plugin for AnalogPlugin<Io>
where
    Io: ExposedIo + AnalogProvider,
    Io::Input: Send,
    Io::Output: Send,
    Io::Error: core::fmt::Display,
    <Io::Input as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
    <Io::Output as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
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
            .register(AnalogPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct AnalogPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> AnalogPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Io> Package for AnalogPackage<Io>
where
    Io: AnalogProvider + Send + Sync + 'static,
    Io::Input: Send,
    Io::Output: Send,
    Io::Error: core::fmt::Display,
    <Io::Input as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
    <Io::Output as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let input_available = Arc::clone(&self.io);
        let input_available_active = Arc::clone(&self.active);
        let output_available = Arc::clone(&self.io);
        let output_available_active = Arc::clone(&self.active);
        let input = Arc::clone(&self.io);
        let input_active = Arc::clone(&self.active);
        let output = Arc::clone(&self.io);
        let output_active = Arc::clone(&self.active);
        lua.register_lib("analog", move |package| {
            package.register("input_available", move |name: String| {
                Some(Ok(input_available_active.load(Ordering::Acquire)
                    && input_available.analog_input_available(&name)))
            })?;
            package.register("output_available", move |name: String| {
                Some(Ok(output_available_active.load(Ordering::Acquire)
                    && output_available.analog_output_available(&name)))
            })?;
            package.register_with("open_input", move |lua, name: String| {
                Some((|| {
                    ensure_active(&input_active)?;
                    let channel = input
                        .acquire_analog_input(&name)
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    lua.create_userdata(AnalogInputHandle::new(channel, Arc::clone(&input_active)))
                })())
            })?;
            package.register_with("open_output", move |lua, name: String| {
                Some((|| {
                    ensure_active(&output_active)?;
                    let channel = output
                        .acquire_analog_output(&name)
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    lua.create_userdata(AnalogOutputHandle::new(
                        channel,
                        Arc::clone(&output_active),
                    ))
                })())
            })
        })
    }
}

impl<Io> LuaPackage for AnalogPackage<Io>
where
    Io: AnalogProvider + Send + Sync + 'static,
    Io::Input: Send,
    Io::Output: Send,
    Io::Error: core::fmt::Display,
    <Io::Input as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
    <Io::Output as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "analog"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct AnalogInputHandle<Channel> {
    channel: Option<Channel>,
    active: Arc<AtomicBool>,
}

impl<Channel> AnalogInputHandle<Channel> {
    fn new(channel: Channel, active: Arc<AtomicBool>) -> Self {
        Self {
            channel: Some(channel),
            active,
        }
    }
    fn close(&mut self) {
        self.channel.take();
    }
}

impl<Channel> UserData for AnalogInputHandle<Channel>
where
    Channel: AnalogInput + Send + 'static,
    Channel::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("max_value", |handle, (): ()| {
            Some(
                handle
                    .channel()
                    .map(|channel| i64::from(channel.max_value())),
            )
        });
        methods.add_method_mut("read", |handle, (): ()| {
            Some((|| {
                let value = handle.channel_mut()?.read().map_err(analog_error)?;
                Ok(i64::from(value))
            })())
        });
        add_common_input_methods(methods);
    }
}

fn add_common_input_methods<Channel>(methods: &mut UserDataMethods<'_, AnalogInputHandle<Channel>>)
where
    Channel: AnalogInput + Send + 'static,
    Channel::Error: core::fmt::Debug,
{
    methods.add_method("is_open", |handle, (): ()| {
        Some(Ok(
            handle.active.load(Ordering::Acquire) && handle.channel.is_some()
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

impl<Channel: AnalogInput> AnalogInputHandle<Channel> {
    fn channel(&self) -> Result<&Channel> {
        ensure_active(&self.active)?;
        self.channel
            .as_ref()
            .ok_or_else(|| Error::runtime("analog input handle is closed"))
    }
    fn channel_mut(&mut self) -> Result<&mut Channel> {
        ensure_active(&self.active)?;
        self.channel
            .as_mut()
            .ok_or_else(|| Error::runtime("analog input handle is closed"))
    }
}

impl<Channel> Drop for AnalogInputHandle<Channel> {
    fn drop(&mut self) {
        self.close();
    }
}

struct AnalogOutputHandle<Channel> {
    channel: Option<Channel>,
    active: Arc<AtomicBool>,
}

impl<Channel> AnalogOutputHandle<Channel> {
    fn new(channel: Channel, active: Arc<AtomicBool>) -> Self {
        Self {
            channel: Some(channel),
            active,
        }
    }
    fn close(&mut self) {
        self.channel.take();
    }
}

impl<Channel> UserData for AnalogOutputHandle<Channel>
where
    Channel: AnalogOutput + Send + 'static,
    Channel::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("max_value", |handle, (): ()| {
            Some(
                handle
                    .channel()
                    .map(|channel| i64::from(channel.max_value())),
            )
        });
        methods.add_method_mut("write", |handle, value: i64| {
            Some((|| {
                let value = u32::try_from(value)
                    .map_err(|_| Error::runtime("analog output value must be non-negative"))?;
                if value > handle.channel()?.max_value() {
                    return Err(Error::runtime("analog output value exceeds max_value"));
                }
                handle.channel_mut()?.write(value).map_err(analog_error)
            })())
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.channel.is_some()
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

impl<Channel: AnalogOutput> AnalogOutputHandle<Channel> {
    fn channel(&self) -> Result<&Channel> {
        ensure_active(&self.active)?;
        self.channel
            .as_ref()
            .ok_or_else(|| Error::runtime("analog output handle is closed"))
    }
    fn channel_mut(&mut self) -> Result<&mut Channel> {
        ensure_active(&self.active)?;
        self.channel
            .as_mut()
            .ok_or_else(|| Error::runtime("analog output handle is closed"))
    }
}

impl<Channel> Drop for AnalogOutputHandle<Channel> {
    fn drop(&mut self) {
        self.close();
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("analog package has been revoked"))
    }
}

fn analog_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("analog operation failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    extern crate std;
    use super::*;
    use core::convert::Infallible;
    struct Channel(u32);
    impl barracuda_board_hal::AnalogErrorType for Channel {
        type Error = Infallible;
    }
    impl AnalogInput for Channel {
        fn max_value(&self) -> u32 {
            4095
        }
        fn read(&mut self) -> core::result::Result<u32, Self::Error> {
            Ok(self.0)
        }
    }
    impl AnalogOutput for Channel {
        fn max_value(&self) -> u32 {
            255
        }
        fn write(&mut self, value: u32) -> core::result::Result<(), Self::Error> {
            self.0 = value;
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
    impl AnalogProvider for Provider {
        type Input = Channel;
        type Output = Channel;
        type Error = OpenError;
        fn analog_input_available(&self, name: &str) -> bool {
            name == "A0"
        }
        fn analog_output_available(&self, name: &str) -> bool {
            name == "A1"
        }
        fn acquire_analog_input(
            &self,
            name: &str,
        ) -> core::result::Result<Self::Input, Self::Error> {
            if name == "A0" {
                Ok(Channel(1234))
            } else {
                Err(OpenError)
            }
        }
        fn acquire_analog_output(
            &self,
            name: &str,
        ) -> core::result::Result<Self::Output, Self::Error> {
            if name == "A1" {
                Ok(Channel(0))
            } else {
                Err(OpenError)
            }
        }
    }
    #[test]
    fn lua_reads_and_writes_analog_channels() {
        let package = AnalogPackage::new(Arc::new(Provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        let result: bool = lua.load("local analog = require('analog')\nlocal input <close> = analog.open_input('A0')\nlocal output <close> = analog.open_output('A1')\noutput:write(42)\nreturn input:read() == 1234 and input:max_value() == 4095 and output:max_value() == 255").eval().expect("use analog");
        assert!(result);
    }
}
