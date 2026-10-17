//! Lua package for the Board's primary built-in LED strip.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::led_strip::{LedStrip, LedStripPeripheral, Rgb8};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataMethods,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

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
        Builtins: LedStripPeripheral<LedStrip = Strip>,
    {
        Self {
            strip: config::peripherals::LED_STRIP_DEVICE
                .and_then(|instance| context.hal.peripherals.take_led_strip(instance)),
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

/// The strip while no handle holds it; a handle gives it back when it closes.
type SharedStrip<Strip> = Arc<critical_section::Mutex<RefCell<Option<Strip>>>>;

struct LedStripPackage<Strip> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    strip: SharedStrip<Strip>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripPackage<Strip> {
    fn new(strip: Option<Strip>) -> Self {
        Self {
            present: strip.is_some(),
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
        let fill_strip = Arc::clone(&self.strip);
        let fill_active = Arc::clone(&self.active);
        let off_strip = Arc::clone(&self.strip);
        let off_active = Arc::clone(&self.active);
        lua.register_lib("light", move |package| {
            package.register("fill", move |color: String| {
                Some(
                    lend(&fill_strip, &fill_active, |strip| {
                        let rgb = parse_color(&color)?;
                        let pixels = alloc::vec![rgb; strip.len()];
                        strip.write(&pixels).map_err(strip_error)
                    })
                    .map(|()| true),
                )
            })?;
            package.register("off", move |(): ()| {
                Some(
                    lend(&off_strip, &off_active, |strip| {
                        strip.clear().map_err(strip_error)
                    })
                    .map(|()| true),
                )
            })?;
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
                    Some(strip) => lua.create_userdata(LedStripHandle::new(
                        strip,
                        Arc::clone(&open_strip),
                        Arc::clone(&open_active),
                    )),
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
        "light"
    }

    fn app_api(&self) -> Option<barracuda_vm_plugin::AppApi> {
        self.present
            .then_some(barracuda_vm_plugin::AppApi::Capability {
                since: 1,
                purpose: "控制灯光",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct LedStripHandle<Strip> {
    strip: Option<Strip>,
    /// Where the strip goes back when the handle closes.
    home: SharedStrip<Strip>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripHandle<Strip> {
    fn new(strip: Strip, home: SharedStrip<Strip>, active: Arc<AtomicBool>) -> Self {
        Self {
            strip: Some(strip),
            home,
            active,
        }
    }

    fn close(&mut self) {
        if let Some(strip) = self.strip.take() {
            critical_section::with(|section| {
                self.home.borrow(section).replace(Some(strip));
            });
        }
    }
}

/// Runs `use` on the strip while no handle holds it.
fn lend<Strip>(
    strip: &SharedStrip<Strip>,
    active: &AtomicBool,
    using: impl FnOnce(&mut Strip) -> Result<()>,
) -> Result<()> {
    ensure_active(active)?;
    critical_section::with(
        |section| match strip.borrow(section).borrow_mut().as_mut() {
            Some(strip) => using(strip),
            None => Err(Error::runtime(
                "built-in LED strip is unavailable or open in a handle",
            )),
        },
    )
}

/// A color as `#RRGGBB` or one of a few names.
fn parse_color(color: &str) -> Result<Rgb8> {
    let named = match color {
        "white" => Some((255, 255, 255)),
        "warm" => Some((255, 170, 90)),
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 255, 0)),
        "blue" => Some((0, 0, 255)),
        "off" | "black" => Some((0, 0, 0)),
        _ => None,
    };
    if let Some((r, g, b)) = named {
        return Ok(Rgb8::new(r, g, b));
    }
    let hex = color.strip_prefix('#').unwrap_or(color);
    let channel = |at: usize| {
        hex.get(at..at + 2)
            .and_then(|digits| u8::from_str_radix(digits, 16).ok())
    };
    match (hex.len(), channel(0), channel(2), channel(4)) {
        (6, Some(r), Some(g), Some(b)) => Ok(Rgb8::new(r, g, b)),
        _ => Err(Error::runtime(
            "a light color is #RRGGBB or white, warm, red, green, blue, or off",
        )),
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
            .load("local leds = require('light')\nlocal strip <close> = leds.open()\nstrip:write('\\001\\002\\003\\004\\005\\006')\nreturn strip:len() == 2")
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
            .load("local leds = require('light')\nlocal strip, err = leds.open()\nreturn not leds.available() and strip == nil and type(err) == 'string'")
            .eval()
            .expect("query strip");
        assert!(result);
    }

    #[test]
    fn fill_and_off_use_the_strip_while_no_handle_holds_it() {
        let package = LedStripPackage::new(Some(TestStrip { pixels: 3 }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = lua
            .load(
                "local light = require('light')\n\
                 assert(light.fill('#FFAA00'))\n\
                 assert(light.fill('warm'))\n\
                 local ok, bad = light.fill('teal')\n\
                 assert(ok == nil and bad:find('#RRGGBB'))\n\
                 local strip = light.open()\n\
                 local busy, err = light.fill('white')\n\
                 assert(busy == nil and err:find('open in a handle'))\n\
                 strip:close()\n\
                 assert(light.available())\n\
                 assert(light.off())\n\
                 local again <close> = assert(light.open())\n\
                 return again:is_open()",
            )
            .eval()
            .expect("use the light");
        assert!(result);
    }

    #[test]
    fn colors_read_as_hex_or_names() {
        assert_eq!(parse_color("#ff8000").ok(), Some(Rgb8::new(255, 128, 0)));
        assert_eq!(parse_color("0000FF").ok(), Some(Rgb8::new(0, 0, 255)));
        assert!(parse_color("#fff").is_err());
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = LedStripPackage::new(Some(TestStrip { pixels: 2 }));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
