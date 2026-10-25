//! Descriptor, color expansion, and dither selection of the PaperColor display.

use barracuda_driver_ed2208::Dither;
use barracuda_paper_color_display::{DESCRIPTOR, dither_for, expand};
use barracuda_peripheral::display::{DisplayFeatures, DisplayTechnology, PixelFormat, RefreshMode};
use embedded_graphics_core::{
    geometry::Size,
    pixelcolor::{IntoStorage, Rgb565},
};

#[test]
fn descriptor_is_a_full_refresh_color_e_paper() {
    assert_eq!(DESCRIPTOR.size(), Size::new(400, 600));
    assert_eq!(DESCRIPTOR.pixel_format(), PixelFormat::Rgb565);
    assert_eq!(DESCRIPTOR.technology(), DisplayTechnology::Epaper);
    assert!(
        !DESCRIPTOR
            .features()
            .contains(DisplayFeatures::PARTIAL_REFRESH)
    );
}

#[test]
fn rgb565_expands_like_m5gfx() {
    assert_eq!(
        expand(Rgb565::new(31, 63, 31).into_storage()),
        [255, 255, 255]
    );
    assert_eq!(
        expand(Rgb565::new(16, 32, 16).into_storage()),
        [132, 130, 132]
    );
}

#[test]
fn refresh_modes_select_m5gfx_dithers() {
    assert_eq!(dither_for(RefreshMode::Full), Dither::Rgb(140));
    assert_eq!(dither_for(RefreshMode::Partial), Dither::Rgb(70));
    assert_eq!(dither_for(RefreshMode::Fast), Dither::Bayer);
}
