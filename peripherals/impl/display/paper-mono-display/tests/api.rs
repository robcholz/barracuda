//! Geometry and gray reduction of the Paper Mono display.

use barracuda_paper_mono_display::{DESCRIPTOR, dithered_level, upright_to_native};
use barracuda_peripheral::display::{DisplayFeatures, DisplayTechnology, PixelFormat};
use embedded_graphics_core::{geometry::Size, pixelcolor::Rgb565};

#[test]
fn descriptor_is_upright_four_gray_with_front_light() {
    assert_eq!(DESCRIPTOR.size(), Size::new(480, 800));
    assert_eq!(DESCRIPTOR.pixel_format(), PixelFormat::Gray2);
    assert_eq!(DESCRIPTOR.technology(), DisplayTechnology::Epaper);
    assert!(DESCRIPTOR.features().contains(DisplayFeatures::BRIGHTNESS));
    assert!(
        !DESCRIPTOR
            .features()
            .contains(DisplayFeatures::PARTIAL_REFRESH)
    );
}

#[test]
fn upright_portrait_maps_to_the_landscape_panel() {
    assert_eq!(upright_to_native(0, 0), (0, 479));
    assert_eq!(upright_to_native(479, 799), (799, 0));
    assert_eq!(upright_to_native(100, 10), (10, 379));
}

#[test]
fn rgb_reduces_to_four_levels() {
    assert_eq!(dithered_level(Rgb565::new(0, 0, 0), 0, 0), 0);
    assert_eq!(dithered_level(Rgb565::new(31, 63, 31), 3, 3), 3);
    let levels: Vec<u8> = (0..4)
        .flat_map(|y| (0..4).map(move |x| dithered_level(Rgb565::new(16, 32, 16), x, y)))
        .collect();
    assert!(levels.iter().all(|level| (1..=2).contains(level)));
}
