//! Lua package for the Board's primary built-in LED strip.

#![no_std]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use barracuda_board_hal::led_strip::{BuiltinLedStrip, LedStrip, Rgb8};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

/// Owns the Board's primary LED-strip capability and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct LedStripPlugin<Strip> {
    strip: Option<Strip>,
}

impl<Strip> LedStripPlugin<Strip> {
    /// Takes the built-in LED strip from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: BuiltinLedStrip<LedStrip = Strip>,
    {
        Self {
            strip: context.hal.builtins.take_led_strip(),
        }
    }
}

impl<Strip> Plugin for LedStripPlugin<Strip>
where
    Strip: LedStrip + Send + 'static,
    Strip::Error: core::fmt::Debug,
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
            .register(LedStripPackage::new(self.strip.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct LedStripPackage<Strip> {
    strip: Arc<critical_section::Mutex<RefCell<Option<Strip>>>>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripPackage<Strip> {
    fn new(strip: Option<Strip>) -> Self {
        Self {
            strip: Arc::new(critical_section::Mutex::new(RefCell::new(strip))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Strip> Package for LedStripPackage<Strip>
where
    Strip: LedStrip + Send + 'static,
    Strip::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_strip = Arc::clone(&self.strip);
        let available_active = Arc::clone(&self.active);
        let open_strip = Arc::clone(&self.strip);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("led_strip", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_strip.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("LED-strip package has been revoked")));
                }
                let strip = critical_section::with(|section| {
                    open_strip.borrow(section).borrow_mut().take()
                });
                Some(match strip {
                    Some(strip) => {
                        lua.create_userdata(LedStripHandle::new(strip, Arc::clone(&open_active)))
                    }
                    None => Err(Error::runtime(
                        "built-in LED strip is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Strip> LuaPackage for LedStripPackage<Strip>
where
    Strip: LedStrip + Send + 'static,
    Strip::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "led_strip"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct LedStripHandle<Strip> {
    strip: Option<Strip>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripHandle<Strip> {
    fn new(strip: Strip, active: Arc<AtomicBool>) -> Self {
        Self {
            strip: Some(strip),
            active,
        }
    }

    fn close(&mut self) {
        self.strip.take();
    }
}

impl<Strip> UserData for LedStripHandle<Strip>
where
    Strip: LedStrip + Send + 'static,
    Strip::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("len", |handle, (): ()| {
            Some(handle.strip().and_then(|strip| {
                i64::try_from(strip.len())
                    .map_err(|_| Error::runtime("LED-strip length is too large"))
            }))
        });
        methods.add_method_mut("write", |handle, bytes: Vec<u8>| Some(handle.write(&bytes)));
        methods.add_method_mut("clear", |handle, (): ()| {
            Some((|| handle.strip_mut()?.clear().map_err(strip_error))())
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.strip.is_some()
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

impl<Strip> LedStripHandle<Strip>
where
    Strip: LedStrip,
    Strip::Error: core::fmt::Debug,
{
    fn strip(&self) -> Result<&Strip> {
        ensure_active(&self.active)?;
        self.strip
            .as_ref()
            .ok_or_else(|| Error::runtime("LED-strip handle is closed"))
    }

    fn strip_mut(&mut self) -> Result<&mut Strip> {
        ensure_active(&self.active)?;
        self.strip
            .as_mut()
            .ok_or_else(|| Error::runtime("LED-strip handle is closed"))
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let (rgb, remainder) = bytes.as_chunks::<3>();
        if !remainder.is_empty() {
            return Err(Error::runtime(
                "LED-strip RGB payload must contain three bytes per pixel",
            ));
        }
        let pixels = rgb
            .iter()
            .map(|rgb| Rgb8::new(rgb[0], rgb[1], rgb[2]))
            .collect::<Vec<_>>();
        if pixels.len() > self.strip()?.len() {
            return Err(Error::runtime(
                "LED-strip payload exceeds the configured pixel count",
            ));
        }
        self.strip_mut()?.write(&pixels).map_err(strip_error)
    }
}

impl<Strip> Drop for LedStripHandle<Strip> {
    fn drop(&mut self) {
        self.close();
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("LED-strip package has been revoked"))
    }
}

fn strip_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("LED-strip operation failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;

    struct TestStrip {
        pixels: usize,
    }

    impl LedStrip for TestStrip {
        type Error = core::convert::Infallible;

        fn len(&self) -> usize {
            self.pixels
        }

        fn write(&mut self, pixels: &[Rgb8]) -> core::result::Result<(), Self::Error> {
            self.pixels = pixels.len();
            Ok(())
        }
    }

    #[test]
    fn lua_takes_and_writes_the_builtin_strip_once() {
        let package = LedStripPackage::new(Some(TestStrip { pixels: 2 }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = lua
            .load("local leds = require('led_strip')\nlocal strip <close> = leds.open()\nstrip:write('\\001\\002\\003\\004\\005\\006')\nreturn strip:len() == 2")
            .eval()
            .expect("use strip");
        assert!(result);
    }

    #[test]
    fn unavailable_board_reports_no_strip() {
        let package = LedStripPackage::<TestStrip>::new(None);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = lua
            .load("local leds = require('led_strip')\nlocal strip, err = leds.open()\nreturn not leds.available() and strip == nil and type(err) == 'string'")
            .eval()
            .expect("query strip");
        assert!(result);
    }
}
