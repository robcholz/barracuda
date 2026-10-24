//! Lua package for the Board's built-in LED strips.

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
    BoardDevices, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package,
    Result, UserData, UserDataMethods, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's LED strips and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct LedStripPlugin<Strip> {
    strips: Vec<(&'static str, Strip)>,
}

impl<Strip> LedStripPlugin<Strip> {
    /// Takes the built-in LED strips from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: LedStripPeripheral<LedStrip = Strip>,
    {
        Self {
            strips: config::peripherals::LED_STRIP_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_led_strip(*instance)
                        .map(|strip| (*name, strip))
                })
                .collect(),
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
            .register(LedStripPackage::new(core::mem::take(&mut self.strips)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct LedStripPackage<Strip> {
    strips: BoardDevices<Strip>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripPackage<Strip> {
    fn new(strips: Vec<(&'static str, Strip)>) -> Self {
        Self {
            strips: BoardDevices::new(strips),
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
        let available = (self.strips.clone(), Arc::clone(&self.active));
        let list = self.strips.clone();
        let open = (self.strips.clone(), Arc::clone(&self.active));
        let fill = (self.strips.clone(), Arc::clone(&self.active));
        let off = (self.strips.clone(), Arc::clone(&self.active));
        lua.register_lib("light", move |package| {
            package.register("fill", move |values: Variadic<String>| {
                let (strips, active) = &fill;
                let values: Vec<String> = values.into();
                Some(
                    match values.as_slice() {
                        [color] => Ok((color, None)),
                        [color, name] => Ok((color, Some(name.as_str()))),
                        _ => Err(Error::runtime(
                            "expected a color and an optional strip name",
                        )),
                    }
                    .and_then(|(color, name)| {
                        lend(strips, active, name, |strip| {
                            let rgb = parse_color(color)?;
                            let pixels = alloc::vec![rgb; strip.len()];
                            strip.write(&pixels).map_err(strip_error)
                        })
                    })
                    .map(|()| true),
                )
            })?;
            package.register("off", move |name: Variadic<String>| {
                let (strips, active) = &off;
                Some(
                    lend(strips, active, name.first().map(String::as_str), |strip| {
                        strip.clear().map_err(strip_error)
                    })
                    .map(|()| true),
                )
            })?;
            package.register("available", move |name: Variadic<String>| {
                let (strips, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && strips.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_with("open", move |lua, name: Variadic<String>| {
                let (strips, active) = &open;
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("LED-strip package has been revoked")));
                }
                let name = name.first().map(String::as_str);
                Some(match strips.lend(name) {
                    Some(strip) => {
                        lua.create_userdata(LedStripHandle::new(strip, Arc::clone(active)))
                    }
                    None => Err(Error::runtime(match name {
                        Some(name) => {
                            alloc::format!("LED strip `{name}` is unavailable or already open")
                        }
                        None => String::from("built-in LED strip is unavailable or already open"),
                    })),
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

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.strips.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 3,
            purpose: "Control lights",
        })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct LedStripHandle<Strip> {
    /// The strip, which goes back to the package when the handle closes.
    strip: Option<LentDevice<Strip>>,
    active: Arc<AtomicBool>,
}

impl<Strip> LedStripHandle<Strip> {
    fn new(strip: LentDevice<Strip>, active: Arc<AtomicBool>) -> Self {
        Self {
            strip: Some(strip),
            active,
        }
    }

    fn close(&mut self) {
        self.strip.take();
    }
}

/// Runs `using` on the strip `name`, or the primary one, while no handle
/// holds it.
fn lend<Strip>(
    strips: &BoardDevices<Strip>,
    active: &AtomicBool,
    name: Option<&str>,
    using: impl FnOnce(&mut Strip) -> Result<()>,
) -> Result<()> {
    ensure_active(active)?;
    match strips.lend(name) {
        Some(mut strip) => using(&mut strip),
        None => Err(Error::runtime(match name {
            Some(name) => alloc::format!("LED strip `{name}` is unavailable or open in a handle"),
            None => String::from("built-in LED strip is unavailable or open in a handle"),
        })),
    }
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
            .as_deref()
            .ok_or_else(|| Error::runtime("LED-strip handle is closed"))
    }

    fn strip_mut(&mut self) -> Result<&mut Strip> {
        ensure_active(&self.active)?;
        self.strip
            .as_deref_mut()
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
        let package = LedStripPackage::new(std::vec![("ring", TestStrip { pixels: 2 })]);
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
        let package = LedStripPackage::<TestStrip>::new(Vec::new());
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
        let package = LedStripPackage::new(std::vec![("ring", TestStrip { pixels: 3 })]);
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
    fn lua_names_a_strip_and_defaults_to_the_primary() {
        let package = LedStripPackage::new(std::vec![
            ("ring", TestStrip { pixels: 3 }),
            ("bar", TestStrip { pixels: 5 }),
        ]);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = lua
            .load(
                "local light = require('light')\n\
                 local names = light.list()\n\
                 assert(#names == 2 and names[1] == 'ring' and names[2] == 'bar')\n\
                 local ring <close> = assert(light.open())\n\
                 assert(light.fill('white', 'bar') and light.off('bar'))\n\
                 local bar <close> = assert(light.open('bar'))\n\
                 local missing, err = light.open('missing')\n\
                 return ring:len() == 3 and bar:len() == 0 and missing == nil and err:find('missing') ~= nil",
            )
            .eval()
            .expect("use both strips");
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
        let package = LedStripPackage::new(std::vec![("ring", TestStrip { pixels: 2 })]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
