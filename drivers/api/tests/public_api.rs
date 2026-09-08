//! External implementation tests for the stable Driver API.

#![allow(clippy::expect_used)]

use std::convert::Infallible;

use barracuda_driver::{
    PeripheralDriver,
    display::{
        BuiltinDisplay, Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation,
        DisplayPower, DisplayTechnology, PixelFormat, RefreshMode, RefreshRequest,
    },
    indicator::Indicator,
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::{Rgb565, Rgb888},
    primitives::Rectangle,
};

#[derive(Default)]
struct CustomDisplay {
    flushed: bool,
    brightness: u8,
    power: Option<DisplayPower>,
    orientation: Option<DisplayOrientation>,
}

impl OriginDimensions for CustomDisplay {
    fn size(&self) -> Size {
        Size::new(320, 240)
    }
}

impl DrawTarget for CustomDisplay {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for _ in pixels {}
        Ok(())
    }
}

impl Display for CustomDisplay {
    type ControlError = Infallible;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BUFFERED
                | DisplayFeatures::PARTIAL_REFRESH
                | DisplayFeatures::BRIGHTNESS,
        )
    }

    fn draw_rgb888(
        &mut self,
        _area: Rectangle,
        _pixels: &[Rgb888],
    ) -> Result<(), Self::RenderError> {
        Ok(())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        self.flushed = true;
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        self.power = Some(power);
        Ok(())
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.brightness = brightness;
        Ok(())
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        self.orientation = Some(orientation);
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

struct CustomDisplayDriver;

impl PeripheralDriver for CustomDisplayDriver {
    type Bindings = ();
    type Config = u8;
    type Capability = CustomDisplay;
    type Error = Infallible;

    async fn initialize(
        _bindings: Self::Bindings,
        brightness: Self::Config,
    ) -> Result<Self::Capability, Self::Error> {
        Ok(CustomDisplay {
            brightness,
            ..CustomDisplay::default()
        })
    }
}

struct Builtins {
    display: Option<CustomDisplay>,
}

impl BuiltinDisplay for Builtins {
    type Display = CustomDisplay;

    fn take_display(&mut self) -> Option<Self::Display> {
        self.display.take()
    }
}

#[test]
fn custom_driver_can_implement_the_stable_display_api() {
    fn assert_driver<D: PeripheralDriver<Capability = CustomDisplay>>() {}
    assert_driver::<CustomDisplayDriver>();
    let display = CustomDisplay {
        brightness: 127,
        ..CustomDisplay::default()
    };
    let mut builtins = Builtins {
        display: Some(display),
    };
    let mut display = builtins.take_display().expect("one display owner");

    let descriptor = display.descriptor();
    assert_eq!(descriptor.size(), Size::new(320, 240));
    assert_eq!(descriptor.pixel_format(), PixelFormat::Rgb565);
    assert_eq!(descriptor.technology(), DisplayTechnology::Lcd);
    assert!(descriptor.features().contains(DisplayFeatures::BUFFERED));
    assert!(
        descriptor
            .features()
            .contains(DisplayFeatures::PARTIAL_REFRESH)
    );
    assert_eq!(display.brightness, 127);

    let _flush = display.flush(RefreshRequest::new(None, RefreshMode::Automatic));
}

#[derive(Default)]
struct CustomIndicator(bool);

impl Indicator for CustomIndicator {
    type Error = Infallible;

    fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        self.0 = enabled;
        Ok(())
    }

    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0)
    }
}

#[test]
fn indicator_defaults_are_part_of_the_stable_api() {
    let mut indicator = CustomIndicator::default();
    indicator.on().expect("indicator on");
    assert!(indicator.is_enabled().expect("indicator state"));
    indicator.off().expect("indicator off");
    assert!(!indicator.is_enabled().expect("indicator state"));
}
