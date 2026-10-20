//! Stable peripheral behavior of direct MIPI displays.

use barracuda_mipi_dbi_display::MipiDbiDisplayConfig;
use barracuda_peripheral::display::{
    DisplayFeatures, DisplayOrientation, DisplayTechnology, PixelFormat,
};
use embedded_graphics_core::geometry::Size;

#[test]
fn configuration_reports_direct_panel_semantics() {
    let config = MipiDbiDisplayConfig::new(
        Size::new(320, 240),
        PixelFormat::Rgb565,
        DisplayOrientation::Deg0,
    );

    let descriptor = config.descriptor();

    assert_eq!(descriptor.size(), Size::new(320, 240));
    assert_eq!(descriptor.pixel_format(), PixelFormat::Rgb565);
    assert_eq!(descriptor.technology(), DisplayTechnology::Lcd);
    assert!(descriptor.features().contains(DisplayFeatures::ORIENTATION));
    assert!(descriptor.features().contains(DisplayFeatures::SLEEP));
    assert!(!descriptor.features().contains(DisplayFeatures::BUFFERED));
}

#[test]
fn rotated_configuration_reports_logical_dimensions() {
    let descriptor = MipiDbiDisplayConfig::new(
        Size::new(135, 240),
        PixelFormat::Rgb565,
        DisplayOrientation::Deg90,
    )
    .descriptor();

    assert_eq!(descriptor.size(), Size::new(240, 135));
}

#[test]
fn digital_backlight_is_advertised_as_brightness_control() {
    let descriptor = MipiDbiDisplayConfig::new(
        Size::new(240, 240),
        PixelFormat::Rgb565,
        DisplayOrientation::Deg0,
    )
    .with_digital_backlight(true)
    .descriptor();

    assert!(descriptor.features().contains(DisplayFeatures::BRIGHTNESS));
}

#[test]
fn digital_backlight_follows_its_active_level() {
    use barracuda_mipi_dbi_display::{DigitalBacklight, PanelBacklight};
    use core::convert::Infallible;
    use embedded_hal::digital::{ErrorType, OutputPin};

    #[derive(Default)]
    struct Pin {
        high: bool,
    }

    impl ErrorType for Pin {
        type Error = Infallible;
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Infallible> {
            self.high = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.high = true;
            Ok(())
        }
    }

    let mut active_low = Pin::default();
    assert!(
        DigitalBacklight::new(&mut active_low, false)
            .set_brightness(128)
            .is_ok()
    );
    assert!(!active_low.high);
    assert!(
        DigitalBacklight::new(&mut active_low, false)
            .set_brightness(0)
            .is_ok()
    );
    assert!(active_low.high);
}
