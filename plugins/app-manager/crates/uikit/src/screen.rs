//! Puts the shown view on the Board display and reads where it is touched.
//!
//! The view is rasterized in strips of [`STRIP_ROWS`] rows, so the Plugin
//! never holds a framebuffer: only the regions the PocketJS damage tracker
//! reports changed since the last frame are drawn, each strip in RGB565, the
//! display API's pixel format.
//!
//! Views are laid out in view pixels, and each view pixel covers `density` ×
//! `density` display pixels: the display's size divided by the density is the
//! size views see.

use crate::DisplayInfo;
use crate::service::{Shown, UiService};
use alloc::vec::Vec;
use barracuda_board_hal::display::{Display, DisplayFeatures, RefreshMode, RefreshRequest};
use barracuda_board_hal::touch::Touch;
use barracuda_ui_runtime::pocketjs_core::{
    Ui as CoreUi,
    damage::{DamagePolicy, DamageRect, DamageTarget, DamageTracker},
    raster::{MAX_RENDER_SCALE, render_scaled_rgb565_window_over},
};
use embedded_graphics_core::{
    geometry::{Point, Size},
    pixelcolor::{Rgb565, raw::RawU16},
    primitives::Rectangle,
};

/// Rows rasterized and sent to the display at a time.
pub(crate) const STRIP_ROWS: i32 = 16;
/// Damage regions tracked per frame before the tracker merges them.
const MAX_REGIONS: usize = 8;
/// Identifies this renderer's target format to the damage tracker.
const SIGNATURE: u64 = u64::from_le_bytes(*b"bcda565\0");

/// The display and what was last drawn on it.
pub(crate) struct Screen<Panel> {
    display: Panel,
    /// The display's size in display pixels.
    width: u32,
    height: u32,
    /// Display pixels per view pixel along each axis.
    density: u32,
    round: bool,
    tracker: DamageTracker<MAX_REGIONS>,
    words: Vec<u32>,
    /// The rasterizer's output for one strip.
    strip: Vec<u16>,
    /// The same strip as the display takes it.
    pixels: Vec<Rgb565>,
    /// What the panel holds: the shown state it was drawn for, or `None`
    /// before the first frame and after the display refused one.
    drawn: Option<Shown>,
    /// Whether the display refused pixels during the current frame.
    refused: bool,
}

impl<Panel: Display> Screen<Panel> {
    /// Draws on `display` at `density` display pixels per view pixel. A
    /// density the rasterizer cannot draw, or one that does not divide the
    /// display's size, draws at 1.
    pub(crate) fn new(display: Panel, density: u8) -> Self {
        let Size { width, height } = display.size();
        let round = display
            .descriptor()
            .features()
            .contains(DisplayFeatures::ROUND);
        let density = u32::from(density);
        let density = if (1..=MAX_RENDER_SCALE).contains(&density)
            && width % density == 0
            && height % density == 0
        {
            density
        } else {
            log::warn!(
                "ui: a {width}×{height} display cannot draw at density {density}; drawing at 1"
            );
            1
        };
        Self {
            display,
            width,
            height,
            density,
            round,
            tracker: DamageTracker::new(),
            words: Vec::new(),
            strip: Vec::new(),
            pixels: Vec::new(),
            drawn: None,
            refused: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn display(&self) -> &Panel {
        &self.display
    }

    /// The display's size in view pixels, density and shape.
    pub(crate) const fn info(&self) -> DisplayInfo {
        DisplayInfo {
            width: self.width / self.density,
            height: self.height / self.density,
            density: self.density as u8,
            round: self.round,
        }
    }

    /// The size views are laid out in: the display's, in view pixels.
    pub(crate) fn viewport(&self) -> (f32, f32) {
        let info = self.info();
        (info.width as f32, info.height as f32)
    }

    /// Sets the panel's brightness.
    pub(crate) async fn set_brightness(&mut self, brightness: u8) {
        if let Err(error) = self.display.set_brightness(brightness).await {
            log::warn!("ui: the display refused a brightness: {error:?}");
        }
    }

    /// Draws what changed since the last frame and presents it.
    ///
    /// Returns whether anything changed or the view moves on its own, so the
    /// caller keeps stepping frames while a view animates or scrolls.
    pub(crate) async fn refresh(&mut self, service: &UiService) -> bool {
        let shown = service.shown();
        self.refused = false;
        if self
            .drawn
            .is_none_or(|drawn| drawn.view != shown.view || drawn.revision != shown.revision)
        {
            self.tracker.invalidate();
        }
        // Scrolled content that carries on needs frames though nothing else
        // changed.
        let mut moving = false;
        let painted = match shown.view {
            Some(view) => service
                .with_document(view, |document| {
                    document.frame();
                    moving = document.is_moving();
                    let core = document.draw(&mut self.words);
                    self.paint(core)
                })
                .unwrap_or_else(|| self.blank(shown)),
            None => self.blank(shown),
        };
        // A refused frame is redrawn whole at the next change.
        self.drawn = (!self.refused).then_some(shown);
        let Some(bounds) = painted else {
            return moving;
        };
        let region = self.area(bounds);
        if let Err(error) = self
            .display
            .flush(RefreshRequest::new(Some(region), RefreshMode::Automatic))
            .await
        {
            log::warn!("ui: the display refused a refresh: {error:?}");
            self.tracker.invalidate();
        }
        true
    }

    /// Rasterizes the damaged regions of `core`'s DrawList; returns their
    /// bounds, or `None` when nothing changed.
    fn paint(&mut self, core: &CoreUi) -> Option<DamageRect> {
        let target = DamageTarget::new(self.width, self.height, self.density, SIGNATURE);
        let plan = match self
            .tracker
            .prepare(core, &self.words, target)
            .and_then(|plan| plan.with_policy(DamagePolicy::default()))
        {
            Ok(plan) => plan,
            Err(error) => {
                log::warn!("ui: cannot draw the view: {error:?}");
                return None;
            }
        };
        if plan.is_empty() {
            return None;
        }
        for region in plan.regions() {
            let mut y0 = region.y0;
            while y0 < region.y1 {
                let strip =
                    DamageRect::new(region.x0, y0, region.x1, (y0 + STRIP_ROWS).min(region.y1));
                self.strip.clear();
                self.strip.resize(
                    rect_pixels(strip) * (self.density * self.density) as usize,
                    0,
                );
                render_scaled_rgb565_window_over(
                    core,
                    &self.words,
                    &mut self.strip,
                    self.density,
                    strip,
                );
                self.pixels.clear();
                self.pixels.extend(
                    self.strip
                        .iter()
                        .map(|&pixel| Rgb565::from(RawU16::new(pixel))),
                );
                if !self.send(strip) {
                    return None;
                }
                y0 = strip.y1;
            }
        }
        self.tracker.commit(core, &self.words, target);
        Some(plan.bounds())
    }

    /// Clears the panel once after the view left it; `None` while it is
    /// already blank.
    fn blank(&mut self, shown: Shown) -> Option<DamageRect> {
        if self
            .drawn
            .is_some_and(|drawn| drawn.view == shown.view && drawn.revision == shown.revision)
        {
            return None;
        }
        let DisplayInfo { width, height, .. } = self.info();
        let screen = DamageRect::new(0, 0, width as i32, height as i32);
        let mut y0 = 0;
        while y0 < screen.y1 {
            let strip = DamageRect::new(0, y0, screen.x1, (y0 + STRIP_ROWS).min(screen.y1));
            self.pixels.clear();
            self.pixels.resize(
                rect_pixels(strip) * (self.density * self.density) as usize,
                Rgb565::new(0, 0, 0),
            );
            if !self.send(strip) {
                return None;
            }
            y0 = strip.y1;
        }
        Some(screen)
    }

    /// The display pixels view-pixel rect `rect` covers.
    fn area(&self, rect: DamageRect) -> Rectangle {
        let density = self.density as i32;
        Rectangle::new(
            Point::new(rect.x0 * density, rect.y0 * density),
            Size::new(
                ((rect.x1 - rect.x0) * density) as u32,
                ((rect.y1 - rect.y0) * density) as u32,
            ),
        )
    }

    /// Sends the staged pixels for view-pixel `strip` to the display.
    fn send(&mut self, strip: DamageRect) -> bool {
        let area = self.area(strip);
        match self.display.draw_rgb565(area, &self.pixels) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("ui: the display refused pixels: {error:?}");
                self.tracker.invalidate();
                self.refused = true;
                false
            }
        }
    }
}

fn rect_pixels(rect: DamageRect) -> usize {
    (rect.x1 - rect.x0) as usize * (rect.y1 - rect.y0) as usize
}

/// The touch controller, read in view coordinates.
pub(crate) struct Pointer<Input> {
    touch: Input,
    scale: (f32, f32),
}

impl<Input: Touch> Pointer<Input> {
    /// Maps the controller's coordinate span onto `display`'s viewport; a
    /// controller that reports no span is taken to match the display.
    pub(crate) fn new(touch: Input, display: DisplayInfo) -> Self {
        let descriptor = touch.descriptor();
        let axis = |span: u16, view: u32| {
            if span == 0 {
                1.0 / f32::from(display.density)
            } else {
                view as f32 / f32::from(span)
            }
        };
        Self {
            scale: (
                axis(descriptor.width, display.width),
                axis(descriptor.height, display.height),
            ),
            touch,
        }
    }

    /// Where the first finger is, or `None` while nothing touches the panel
    /// or the controller cannot be read.
    pub(crate) fn poll(&mut self) -> Option<(f32, f32)> {
        let frame = match self.touch.read_frame() {
            Ok(frame) => frame,
            Err(error) => {
                log::warn!("ui: cannot read touch: {error:?}");
                return None;
            }
        };
        frame.get(0).map(|point| {
            (
                f32::from(point.x) * self.scale.0,
                f32::from(point.y) * self.scale.1,
            )
        })
    }
}
