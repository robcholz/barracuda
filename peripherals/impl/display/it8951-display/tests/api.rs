//! Geometry, mounting, and gray reduction of the IT8951 display.

use barracuda_it8951_display::{It8951DisplayConfig, dithered_level};
use barracuda_peripheral::display::{
    DisplayFeatures, DisplayOrientation, DisplayTechnology, PixelFormat,
};
use embedded_graphics_core::{geometry::Size, pixelcolor::Rgb565};

#[test]
fn m5paper_is_upright_portrait_with_sixteen_gray_levels() {
    let config = It8951DisplayConfig::new(960, 540, DisplayOrientation::Deg270);
    let descriptor = config.descriptor();
    assert_eq!(descriptor.size(), Size::new(540, 960));
    assert_eq!(descriptor.pixel_format(), PixelFormat::Gray4);
    assert_eq!(descriptor.technology(), DisplayTechnology::Epaper);
    assert!(descriptor.features().contains(DisplayFeatures::BUFFERED));
    assert!(
        descriptor
            .features()
            .contains(DisplayFeatures::PARTIAL_REFRESH)
    );
    assert!(!descriptor.features().contains(DisplayFeatures::BRIGHTNESS));
}

#[test]
fn mounting_follows_m5gfx_rotate_pos() {
    // M5GFX offset_rotation 3: swap, then mirror the panel's Y.
    let config = It8951DisplayConfig::new(960, 540, DisplayOrientation::Deg270);
    assert_eq!(config.to_native(0, 0), (0, 539));
    assert_eq!(config.to_native(539, 959), (959, 0));
    assert_eq!(config.to_native(10, 20), (20, 529));
    let upright = It8951DisplayConfig::new(960, 540, DisplayOrientation::Deg0);
    assert_eq!(upright.to_native(10, 20), (10, 20));
    let flipped = It8951DisplayConfig::new(960, 540, DisplayOrientation::Deg180);
    assert_eq!(flipped.to_native(0, 0), (959, 539));
}

#[test]
fn rgb_is_reduced_to_sixteen_levels() {
    assert_eq!(dithered_level(Rgb565::new(0, 0, 0), 0, 0), 0);
    assert_eq!(dithered_level(Rgb565::new(31, 63, 31), 0, 0), 15);
    // Mid gray straddles levels 7 and 8 across the dither matrix.
    let levels: Vec<u8> = (0..4)
        .flat_map(|y| (0..4).map(move |x| dithered_level(Rgb565::new(16, 32, 16), x, y)))
        .collect();
    assert!(levels.iter().all(|level| (7..=8).contains(level)));
    assert!(levels.contains(&7) && levels.contains(&8));
}
