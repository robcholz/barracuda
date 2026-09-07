//! Stable capability metadata for the GDEH0154D67 panel.

use barracuda_driver::display::{DisplayFeatures, DisplayTechnology, PixelFormat};
use barracuda_gdeh0154d67_display::DESCRIPTOR;
use embedded_graphics_core::geometry::Size;

#[test]
fn descriptor_reports_buffered_e_paper_semantics() {
    assert_eq!(DESCRIPTOR.size(), Size::new(200, 200));
    assert_eq!(DESCRIPTOR.pixel_format(), PixelFormat::Binary);
    assert_eq!(DESCRIPTOR.technology(), DisplayTechnology::Epaper);
    assert!(DESCRIPTOR.features().contains(DisplayFeatures::BUFFERED));
    assert!(DESCRIPTOR.features().contains(DisplayFeatures::BUSY));
    assert!(DESCRIPTOR.features().contains(DisplayFeatures::SLEEP));
    assert!(!DESCRIPTOR.features().contains(DisplayFeatures::BRIGHTNESS));
}
