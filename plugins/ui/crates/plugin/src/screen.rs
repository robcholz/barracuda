//! Puts the shown view on the Board display and reads where it is touched.
//!
//! The view is rasterized in strips of [`STRIP_ROWS`] rows, so the Plugin
//! never holds a framebuffer: only the regions the PocketJS damage tracker
//! reports changed since the last frame are drawn, each strip in RGB565, the
//! display API's pixel format.

use crate::service::{Shown, UiService};
use alloc::vec::Vec;
use barracuda_board_hal::display::{Display, RefreshMode, RefreshRequest};
use barracuda_board_hal::touch::Touch;
use barracuda_ui_runtime::pocketjs_core::{
    Ui as CoreUi,
    damage::{DamagePolicy, DamageRect, DamageTarget, DamageTracker},
    raster::render_scaled_rgb565_window_over,
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
    width: u32,
    height: u32,
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
    pub(crate) fn new(display: Panel) -> Self {
        let Size { width, height } = display.size();
        Self {
            display,
            width,
            height,
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

    /// The size views are laid out in: the display's, one view pixel per
    /// display pixel.
    pub(crate) fn viewport(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }

    /// Sets the panel's brightness.
    pub(crate) async fn set_brightness(&mut self, brightness: u8) {
        if let Err(error) = self.display.set_brightness(brightness).await {
            log::warn!("ui: the display refused a brightness: {error:?}");
        }
    }

    /// Draws what changed since the last frame and presents it.
    ///
    /// Returns whether anything changed, so the caller keeps stepping frames
    /// while a view animates.
    pub(crate) async fn refresh(&mut self, service: &UiService) -> bool {
        let shown = service.shown();
        self.refused = false;
        if self
            .drawn
            .is_none_or(|drawn| drawn.view != shown.view || drawn.revision != shown.revision)
        {
            self.tracker.invalidate();
        }
        let painted = match shown.view {
            Some(view) => service
                .with_document(view, |document| {
                    document.frame();
                    let core = document.draw(&mut self.words);
                    self.paint(core)
                })
                .unwrap_or_else(|| self.blank(shown)),
            None => self.blank(shown),
        };
        // A refused frame is redrawn whole at the next change.
        self.drawn = (!self.refused).then_some(shown);
        let Some(bounds) = painted else {
            return false;
        };
        let region = Rectangle::new(
            Point::new(bounds.x0, bounds.y0),
            Size::new(
                (bounds.x1 - bounds.x0) as u32,
                (bounds.y1 - bounds.y0) as u32,
            ),
        );
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
        let target = DamageTarget::new(self.width, self.height, 1, SIGNATURE);
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
                self.strip.resize(rect_pixels(strip), 0);
                render_scaled_rgb565_window_over(core, &self.words, &mut self.strip, 1, strip);
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
        let screen = DamageRect::new(0, 0, self.width as i32, self.height as i32);
        let mut y0 = 0;
        while y0 < screen.y1 {
            let strip = DamageRect::new(0, y0, screen.x1, (y0 + STRIP_ROWS).min(screen.y1));
            self.pixels.clear();
            self.pixels.resize(rect_pixels(strip), Rgb565::new(0, 0, 0));
            if !self.send(strip) {
                return None;
            }
            y0 = strip.y1;
        }
        Some(screen)
    }

    /// Sends the staged pixels for `strip` to the display.
    fn send(&mut self, strip: DamageRect) -> bool {
        let area = Rectangle::new(
            Point::new(strip.x0, strip.y0),
            Size::new((strip.x1 - strip.x0) as u32, (strip.y1 - strip.y0) as u32),
        );
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
    /// Maps the controller's coordinate span onto `viewport`; a controller
    /// that reports no span is taken to match it.
    pub(crate) fn new(touch: Input, viewport: (f32, f32)) -> Self {
        let descriptor = touch.descriptor();
        let axis = |span: u16, view: f32| {
            if span == 0 {
                1.0
            } else {
                view / f32::from(span)
            }
        };
        Self {
            scale: (
                axis(descriptor.width, viewport.0),
                axis(descriptor.height, viewport.1),
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
