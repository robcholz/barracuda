//! Lua package for the Board's primary built-in display.

#![no_std]

extern crate alloc;

use alloc::string::String;
use barracuda_board_hal::display::{
    Display, DisplayDescriptor, DisplayOrientation, DisplayPeripheral, DisplayPower,
    DisplayTechnology, PixelFormat, RefreshMode, RefreshRequest,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::cell::RefCell;
use embedded_graphics_core::{
    geometry::{Point, Size},
    pixelcolor::Rgb888,
    primitives::Rectangle,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Largest RGB888 region accepted per draw: a full 360x360 frame fits, and the
/// Lua string holding it fits the 1 MiB per-run Lua heap.
const MAX_DRAW_BYTES: usize = 512 * 1024;
/// Pixels converted per implementation call, staged on the stack.
const DRAW_CHUNK_PIXELS: usize = 512;

/// Owns the Board's primary display capability and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct DisplayPlugin<Device> {
    display: Option<Device>,
}

impl<Device> DisplayPlugin<Device> {
    /// Takes the built-in display from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: DisplayPeripheral<Display = Device>,
    {
        Self {
            display: context.hal.peripherals.take_display(),
        }
    }
}

impl<Device> Plugin for DisplayPlugin<Device>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
    Device::RenderError: core::fmt::Debug,
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
            .register(DisplayPackage::new(self.display.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct DisplayPackage<Device> {
    display: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> DisplayPackage<Device> {
    fn new(display: Option<Device>) -> Self {
        Self {
            display: Arc::new(critical_section::Mutex::new(RefCell::new(display))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for DisplayPackage<Device>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
    Device::RenderError: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_display = Arc::clone(&self.display);
        let available_active = Arc::clone(&self.active);
        let open_display = Arc::clone(&self.display);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("display", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_display.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("display package has been revoked")));
                }
                let display = critical_section::with(|section| {
                    open_display.borrow(section).borrow_mut().take()
                });
                Some(match display {
                    Some(display) => {
                        lua.create_userdata(DisplayHandle::new(display, Arc::clone(&open_active)))
                    }
                    None => Err(Error::runtime(
                        "built-in display is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for DisplayPackage<Device>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
    Device::RenderError: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "display"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct DisplayHandle<Device> {
    display: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> DisplayHandle<Device> {
    fn new(display: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            display: Some(display),
            active,
        }
    }

    fn close(&mut self) {
        self.display.take();
    }
}

impl<Device> UserData for DisplayHandle<Device>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
    Device::RenderError: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("descriptor", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_method_mut(
            "draw_rgb888",
            |handle, (x, y, width, height, bytes): (i64, i64, i64, i64, Bytes)| {
                Some(handle.draw_rgb888(x, y, width, height, &bytes))
            },
        );
        methods.add_async_method("flush", |handle, mode: String| async move {
            Some(display_flush(handle, mode).await)
        });
        methods.add_async_method("set_power", |handle, power: String| async move {
            Some(display_set_power(handle, power).await)
        });
        methods.add_async_method("set_brightness", |handle, brightness: i64| async move {
            Some(display_set_brightness(handle, brightness).await)
        });
        methods.add_async_method(
            "set_orientation",
            |handle, orientation: String| async move {
                Some(display_set_orientation(handle, orientation).await)
            },
        );
        methods.add_async_method("wait_ready", |handle, (): ()| async move {
            Some(display_wait_ready(handle).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.display.is_some()
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

impl<Device> DisplayHandle<Device>
where
    Device: Display,
    Device::RenderError: core::fmt::Debug,
{
    fn display(&self) -> Result<&Device> {
        ensure_active(&self.active)?;
        self.display
            .as_ref()
            .ok_or_else(|| Error::runtime("display handle is closed"))
    }

    fn display_mut(&mut self) -> Result<&mut Device> {
        ensure_active(&self.active)?;
        self.display
            .as_mut()
            .ok_or_else(|| Error::runtime("display handle is closed"))
    }

    fn descriptor(&self) -> Result<DisplayDescriptor> {
        Ok(self.display()?.descriptor())
    }

    fn draw_rgb888(&mut self, x: i64, y: i64, width: i64, height: i64, bytes: &[u8]) -> Result<()> {
        let x = i32::try_from(x).map_err(|_| Error::runtime("display x is out of range"))?;
        let y = i32::try_from(y).map_err(|_| Error::runtime("display y is out of range"))?;
        let width = u32::try_from(width)
            .map_err(|_| Error::runtime("display width must be non-negative"))?;
        let height = u32::try_from(height)
            .map_err(|_| Error::runtime("display height must be non-negative"))?;
        if width == 0 || height == 0 {
            return Err(Error::runtime("display region must not be empty"));
        }
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or_else(|| Error::runtime("display region is too large"))?;
        let expected = pixels
            .checked_mul(3)
            .ok_or_else(|| Error::runtime("display RGB payload is too large"))?;
        if expected > MAX_DRAW_BYTES || bytes.len() != expected {
            return Err(Error::runtime(
                "display RGB payload length must equal width * height * 3 and stay within 512 KiB",
            ));
        }
        let (rgb, remainder) = bytes.as_chunks::<3>();
        if !remainder.is_empty() {
            return Err(Error::runtime("display RGB payload is incomplete"));
        }
        draw_region(self.display_mut()?, x, y, width, rgb)
    }
}

/// Draws a validated RGB888 region through a fixed stack buffer: whole rows
/// when they fit and row segments otherwise, so no frame-sized copy is made.
fn draw_region<D: Display>(
    display: &mut D,
    x: i32,
    y: i32,
    width: u32,
    rgb: &[[u8; 3]],
) -> Result<()>
where
    D::RenderError: core::fmt::Debug,
{
    let row_pixels =
        usize::try_from(width).map_err(|_| Error::runtime("display region is too large"))?;
    if row_pixels <= DRAW_CHUNK_PIXELS {
        let rows_per_block = DRAW_CHUNK_PIXELS / row_pixels;
        for (index, block) in rgb.chunks(rows_per_block * row_pixels).enumerate() {
            let origin = Point::new(x, offset(y, index * rows_per_block)?);
            let size = Size::new(width, region_length(block.len() / row_pixels)?);
            draw_block(display, Rectangle::new(origin, size), block)?;
        }
    } else {
        for (row, pixels) in rgb.chunks(row_pixels).enumerate() {
            for (segment, block) in pixels.chunks(DRAW_CHUNK_PIXELS).enumerate() {
                let origin = Point::new(offset(x, segment * DRAW_CHUNK_PIXELS)?, offset(y, row)?);
                let size = Size::new(region_length(block.len())?, 1);
                draw_block(display, Rectangle::new(origin, size), block)?;
            }
        }
    }
    Ok(())
}

/// Converts at most [`DRAW_CHUNK_PIXELS`] RGB888 pixels and draws them.
fn draw_block<D: Display>(display: &mut D, area: Rectangle, block: &[[u8; 3]]) -> Result<()>
where
    D::RenderError: core::fmt::Debug,
{
    let mut colors = [Rgb888::new(0, 0, 0); DRAW_CHUNK_PIXELS];
    for (color, rgb) in colors.iter_mut().zip(block) {
        *color = Rgb888::new(rgb[0], rgb[1], rgb[2]);
    }
    display
        .draw_rgb888(area, &colors[..block.len()])
        .map_err(|error| Error::runtime(alloc::format!("display drawing failed: {error:?}")))
}

fn region_length(length: usize) -> Result<u32> {
    u32::try_from(length).map_err(|_| Error::runtime("display region is too large"))
}

fn offset(origin: i32, delta: usize) -> Result<i32> {
    i32::try_from(delta)
        .ok()
        .and_then(|delta| origin.checked_add(delta))
        .ok_or_else(|| Error::runtime("display region is out of range"))
}

impl<Device> Drop for DisplayHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

async fn display_flush<Device>(
    handle: UserDataHandle<DisplayHandle<Device>>,
    mode: String,
) -> Result<()>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
{
    let mode = parse_refresh_mode(&mode)?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    handle
        .display
        .as_mut()
        .ok_or_else(|| Error::runtime("display handle is closed"))?
        .flush(RefreshRequest::new(None, mode))
        .await
        .map_err(control_error)
}

async fn display_set_power<Device>(
    handle: UserDataHandle<DisplayHandle<Device>>,
    power: String,
) -> Result<()>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
{
    let power = parse_power(&power)?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    handle
        .display
        .as_mut()
        .ok_or_else(|| Error::runtime("display handle is closed"))?
        .set_power(power)
        .await
        .map_err(control_error)
}

async fn display_set_brightness<Device>(
    handle: UserDataHandle<DisplayHandle<Device>>,
    brightness: i64,
) -> Result<()>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
{
    let brightness = u8::try_from(brightness)
        .map_err(|_| Error::runtime("display brightness must be between 0 and 255"))?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    handle
        .display
        .as_mut()
        .ok_or_else(|| Error::runtime("display handle is closed"))?
        .set_brightness(brightness)
        .await
        .map_err(control_error)
}

async fn display_set_orientation<Device>(
    handle: UserDataHandle<DisplayHandle<Device>>,
    orientation: String,
) -> Result<()>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
{
    let orientation = parse_orientation(&orientation)?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    handle
        .display
        .as_mut()
        .ok_or_else(|| Error::runtime("display handle is closed"))?
        .set_orientation(orientation)
        .await
        .map_err(control_error)
}

async fn display_wait_ready<Device>(handle: UserDataHandle<DisplayHandle<Device>>) -> Result<()>
where
    Device: Display + Send + 'static,
    Device::ControlError: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    handle
        .display
        .as_mut()
        .ok_or_else(|| Error::runtime("display handle is closed"))?
        .wait_ready()
        .await
        .map_err(control_error)
}

fn descriptor_values(descriptor: DisplayDescriptor) -> (i64, i64, &'static str, &'static str) {
    (
        i64::from(descriptor.size().width),
        i64::from(descriptor.size().height),
        match descriptor.pixel_format() {
            PixelFormat::Binary => "binary",
            PixelFormat::Gray2 => "gray2",
            PixelFormat::Gray4 => "gray4",
            PixelFormat::Gray8 => "gray8",
            PixelFormat::Rgb565 => "rgb565",
            PixelFormat::Rgb666 => "rgb666",
            PixelFormat::Rgb888 => "rgb888",
            _ => "unknown",
        },
        match descriptor.technology() {
            DisplayTechnology::Lcd => "lcd",
            DisplayTechnology::Oled => "oled",
            DisplayTechnology::Epaper => "epaper",
            DisplayTechnology::LedMatrix => "led-matrix",
            DisplayTechnology::VideoOutput => "video-output",
            _ => "unknown",
        },
    )
}

fn parse_refresh_mode(value: &str) -> Result<RefreshMode> {
    match value {
        "automatic" => Ok(RefreshMode::Automatic),
        "full" => Ok(RefreshMode::Full),
        "partial" => Ok(RefreshMode::Partial),
        "fast" => Ok(RefreshMode::Fast),
        _ => Err(Error::runtime(
            "display refresh mode must be automatic, full, partial, or fast",
        )),
    }
}

fn parse_power(value: &str) -> Result<DisplayPower> {
    match value {
        "on" => Ok(DisplayPower::On),
        "sleep" => Ok(DisplayPower::Sleep),
        "off" => Ok(DisplayPower::Off),
        _ => Err(Error::runtime("display power must be on, sleep, or off")),
    }
}

fn parse_orientation(value: &str) -> Result<DisplayOrientation> {
    match value {
        "deg0" => Ok(DisplayOrientation::Deg0),
        "deg90" => Ok(DisplayOrientation::Deg90),
        "deg180" => Ok(DisplayOrientation::Deg180),
        "deg270" => Ok(DisplayOrientation::Deg270),
        _ => Err(Error::runtime(
            "display orientation must be deg0, deg90, deg180, or deg270",
        )),
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("display package has been revoked"))
    }
}

fn control_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("display control failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;
    use embedded_graphics_core::{Pixel, draw_target::DrawTarget, geometry::OriginDimensions};

    struct TestDisplay {
        drawn: usize,
        areas: std::vec::Vec<Rectangle>,
    }

    impl OriginDimensions for TestDisplay {
        fn size(&self) -> Size {
            Size::new(2, 1)
        }
    }

    impl DrawTarget for TestDisplay {
        type Color = Rgb888;
        type Error = Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> core::result::Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            self.drawn += pixels.into_iter().count();
            Ok(())
        }
    }

    impl Display for TestDisplay {
        type ControlError = Infallible;
        type RenderError = Infallible;

        fn descriptor(&self) -> DisplayDescriptor {
            DisplayDescriptor::new(
                self.size(),
                PixelFormat::Rgb888,
                DisplayTechnology::Lcd,
                Default::default(),
            )
        }

        fn draw_rgb888(
            &mut self,
            area: Rectangle,
            pixels: &[Rgb888],
        ) -> core::result::Result<(), Self::RenderError> {
            assert_eq!(
                area.size.width as usize * area.size.height as usize,
                pixels.len()
            );
            self.areas.push(area);
            self.drawn += pixels.len();
            Ok(())
        }

        async fn flush(
            &mut self,
            _request: RefreshRequest,
        ) -> core::result::Result<(), Self::ControlError> {
            Ok(())
        }

        async fn set_power(
            &mut self,
            _power: DisplayPower,
        ) -> core::result::Result<(), Self::ControlError> {
            Ok(())
        }

        async fn set_brightness(
            &mut self,
            _brightness: u8,
        ) -> core::result::Result<(), Self::ControlError> {
            Ok(())
        }

        async fn set_orientation(
            &mut self,
            _orientation: DisplayOrientation,
        ) -> core::result::Result<(), Self::ControlError> {
            Ok(())
        }

        async fn wait_ready(&mut self) -> core::result::Result<(), Self::ControlError> {
            Ok(())
        }
    }

    #[test]
    fn lua_draws_and_flushes_the_builtin_display() {
        let package = DisplayPackage::new(Some(TestDisplay {
            drawn: 0,
            areas: std::vec::Vec::new(),
        }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local displays = require('display')\nlocal display <close> = displays.open()\nlocal width, height, format, technology = display:descriptor()\ndisplay:draw_rgb888(0, 0, 2, 1, '\\255\\000\\000\\000\\255\\000')\ndisplay:flush('automatic')\nreturn width == 2 and height == 1 and format == 'rgb888' and technology == 'lcd'")
                .eval_async(),
        )
        .expect("use display");
        assert!(result);
    }

    #[test]
    fn regions_draw_through_bounded_stack_blocks() {
        let mut display = TestDisplay {
            drawn: 0,
            areas: std::vec::Vec::new(),
        };
        let rows = std::vec![[1_u8, 2, 3]; 300 * 4];
        super::draw_region(&mut display, 5, 7, 300, &rows).expect("draw rows");
        assert_eq!(display.drawn, 1200);
        assert_eq!(
            display.areas,
            std::vec![
                Rectangle::new(Point::new(5, 7), Size::new(300, 1)),
                Rectangle::new(Point::new(5, 8), Size::new(300, 1)),
                Rectangle::new(Point::new(5, 9), Size::new(300, 1)),
                Rectangle::new(Point::new(5, 10), Size::new(300, 1)),
            ]
        );

        display.areas.clear();
        let wide = std::vec![[0_u8; 3]; 600 * 2];
        super::draw_region(&mut display, 0, 0, 600, &wide).expect("draw wide rows");
        assert_eq!(
            display.areas,
            std::vec![
                Rectangle::new(Point::new(0, 0), Size::new(512, 1)),
                Rectangle::new(Point::new(512, 0), Size::new(88, 1)),
                Rectangle::new(Point::new(0, 1), Size::new(512, 1)),
                Rectangle::new(Point::new(512, 1), Size::new(88, 1)),
            ]
        );

        display.areas.clear();
        let narrow = std::vec![[0_u8; 3]; 100 * 12];
        super::draw_region(&mut display, 0, 0, 100, &narrow).expect("draw narrow rows");
        assert_eq!(
            display.areas,
            std::vec![
                Rectangle::new(Point::new(0, 0), Size::new(100, 5)),
                Rectangle::new(Point::new(0, 5), Size::new(100, 5)),
                Rectangle::new(Point::new(0, 10), Size::new(100, 2)),
            ]
        );
    }
}
