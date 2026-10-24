//! Stable peripheral metadata and controller selection for the 1.54-inch panel.

use barracuda_epaper_1in54_display::{DESCRIPTOR, PanelController};
use barracuda_peripheral::display::{DisplayFeatures, DisplayTechnology, PixelFormat};
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

#[test]
fn idle_busy_polarity_selects_the_controller() {
    // SSD1681: BUSY is active high, so an idle panel reads low.
    assert_eq!(
        PanelController::from_idle_busy_level(false),
        PanelController::Ssd1681
    );
    // JD79653A: BUSY_N is active low, so an idle panel reads high.
    assert_eq!(
        PanelController::from_idle_busy_level(true),
        PanelController::Jd79653a
    );
}
