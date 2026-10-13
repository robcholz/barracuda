#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

extern crate std;

use crate::datum::{ViewType, ViewValue};
use crate::service::{DEFAULT_VIEWPORT, UiAssets, UiService, UiSession};
use crate::view::{MAX_ARRAY_ITEMS, ViewChannel, ViewError};
use alloc::{string::String, vec, vec::Vec};
use futures_lite::future;

const VIEW: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.bvir");
const STYLES: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.styles.bin");
const FONTS: [&[u8]; 3] = [
    include_bytes!("../../runtime/tests/fixtures/counter.font-1.bin"),
    include_bytes!("../../runtime/tests/fixtures/counter.font-2.bin"),
    include_bytes!("../../runtime/tests/fixtures/counter.font-12.bin"),
];

pub(super) fn assets() -> UiAssets {
    UiAssets {
        view: VIEW.to_vec(),
        styles: STYLES.to_vec(),
        fonts: FONTS.iter().map(|font| font.to_vec()).collect(),
    }
}

fn entry(id: i32, label: &str) -> ViewValue {
    ViewValue::List(vec![ViewValue::I32(id), ViewValue::Str(label.into())])
}

fn next_call(channel: &ViewChannel) -> Option<(String, Vec<ViewValue>)> {
    future::block_on(channel.next()).map(|call| (call.handler, call.args))
}

/// Finds a point that presses `handler`; every probe that hits queues a call.
/// "Reset" sits right of "+1", so it is searched for from the right.
pub(super) fn find(session: &UiSession, handler: &str) -> (f32, f32) {
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            let x = if handler == "reset" { 354 - x } else { x };
            let (x, y) = (x as f32, y as f32);
            if session.press_at(x, y).as_deref() == Some(handler) {
                return (x, y);
            }
        }
    }
    panic!("no '{handler}' button")
}

#[test]
fn writes_drive_the_view_and_taps_arrive_as_calls() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);

    channel.set("count", ViewValue::I32(2)).expect("count");
    channel
        .set(
            "history",
            ViewValue::List(vec![entry(2, "two"), entry(1, "one")]),
        )
        .expect("history");
    assert_eq!(session.texts(), ["2", "+1", "Reset", "two", "one"]);

    find(&session, "increment");
    assert_eq!(
        next_call(&channel),
        Some(("increment".into(), vec![ViewValue::I32(1)]))
    );
}

#[test]
fn writes_are_checked_against_the_contract() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel();
    assert_eq!(channel.value_type("count"), Some(&ViewType::I32));
    assert_eq!(channel.value_type("missing"), None);
    for (name, value, error) in [
        ("count", ViewValue::Str("two".into()), ViewError::Mismatch),
        ("count", ViewValue::F64(1.0), ViewError::Mismatch),
        ("missing", ViewValue::I32(1), ViewError::UnknownValue),
        (
            "history",
            ViewValue::List(vec![ViewValue::List(vec![
                ViewValue::Str("x".into()),
                ViewValue::Str("y".into()),
            ])]),
            ViewError::Mismatch,
        ),
        ("history", ViewValue::I32(3), ViewError::Mismatch),
        (
            "history",
            ViewValue::List(vec![entry(1, "one"); MAX_ARRAY_ITEMS + 1]),
            ViewError::Mismatch,
        ),
    ] {
        assert_eq!(
            channel.set(name, value.clone()),
            Err(error),
            "{name} {value:?}"
        );
    }
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn closing_ends_the_view_for_its_owner() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    assert!(!session.is_closed());
    channel.close();
    assert!(session.is_closed());
    assert!(!channel.is_open());
    assert_eq!(
        channel.set("count", ViewValue::I32(1)),
        Err(ViewError::Closed)
    );
    assert_eq!(next_call(&channel), None);
    assert_eq!(session.press_at(0.0, 0.0), None);

    // Dropping the session closes channels that outlive it.
    let other = service.open(&assets()).expect("view opens");
    let channel = other.channel().clone();
    drop(other);
    assert!(!channel.is_open());
}

#[test]
fn a_suspended_view_keeps_its_values_and_catches_up_on_resume() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    channel.set("count", ViewValue::I32(1)).expect("count");
    channel
        .set("history", ViewValue::List(vec![entry(1, "one")]))
        .expect("history");
    assert_eq!(session.texts(), ["1", "+1", "Reset", "one"]);

    session.suspend();
    assert!(session.is_suspended());
    assert!(session.texts().is_empty());
    assert_eq!(session.press_at(180.0, 180.0), None);
    // The owner keeps writing while suspended; the writes wait for the resume.
    channel.set("count", ViewValue::I32(5)).expect("count");

    session.resume(&assets()).expect("resume");
    assert!(!session.is_suspended());
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);

    // Rejected assets leave the view suspended with its values.
    session.suspend();
    let mut broken = assets();
    broken.view.truncate(10);
    assert!(session.resume(&broken).is_err());
    assert!(session.is_suspended());
    session.resume(&assets()).expect("resume");
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);
}

#[test]
fn clearing_drops_calls_the_owner_has_not_received() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    find(&session, "increment");
    session.clear_events();
    find(&session, "reset");
    assert_eq!(
        next_call(session.channel()).map(|(handler, _)| handler),
        Some("reset".into())
    );
}

mod screen {
    use super::{assets, find};
    use crate::screen::{Pointer, Screen};
    use crate::service::{DEFAULT_VIEWPORT, UiService};
    use alloc::{collections::VecDeque, vec, vec::Vec};
    use barracuda_board_hal::display::{
        Display, DisplayDescriptor, DisplayOrientation, DisplayPower, DisplayTechnology,
        PixelFormat, RefreshRequest,
    };
    use barracuda_board_hal::touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint};
    use core::convert::Infallible;
    use embedded_graphics_core::{
        Pixel,
        draw_target::DrawTarget,
        geometry::{OriginDimensions, Size},
        pixelcolor::Rgb888,
        pixelcolor::RgbColor,
        primitives::Rectangle,
    };
    use futures_lite::future;

    const BLACK: Rgb888 = Rgb888::new(0, 0, 0);

    /// A display that keeps its pixels and records every draw and refresh.
    struct Panel {
        width: u32,
        pixels: Vec<Rgb888>,
        drawn: Vec<Rectangle>,
        flushed: Vec<RefreshRequest>,
    }

    impl Panel {
        fn new((width, height): (f32, f32)) -> Self {
            Self {
                width: width as u32,
                pixels: vec![Rgb888::new(1, 2, 3); width as usize * height as usize],
                drawn: Vec::new(),
                flushed: Vec::new(),
            }
        }

        fn drawn_pixels(&self) -> u32 {
            self.drawn
                .iter()
                .map(|area| area.size.width * area.size.height)
                .sum()
        }
    }

    impl OriginDimensions for Panel {
        fn size(&self) -> Size {
            Size::new(self.width, self.pixels.len() as u32 / self.width)
        }
    }

    impl DrawTarget for Panel {
        type Color = Rgb888;
        type Error = Infallible;

        fn draw_iter<I>(&mut self, _pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            Ok(())
        }
    }

    impl Display for Panel {
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

        fn draw_rgb888(&mut self, area: Rectangle, pixels: &[Rgb888]) -> Result<(), Infallible> {
            assert_eq!((area.size.width * area.size.height) as usize, pixels.len());
            for (row, line) in pixels.chunks(area.size.width as usize).enumerate() {
                let start = (area.top_left.y as usize + row) * self.width as usize
                    + area.top_left.x as usize;
                self.pixels[start..start + line.len()].copy_from_slice(line);
            }
            self.drawn.push(area);
            Ok(())
        }

        async fn flush(&mut self, request: RefreshRequest) -> Result<(), Infallible> {
            self.flushed.push(request);
            Ok(())
        }

        async fn set_power(&mut self, _power: DisplayPower) -> Result<(), Infallible> {
            Ok(())
        }

        async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Infallible> {
            Ok(())
        }

        async fn set_orientation(
            &mut self,
            _orientation: DisplayOrientation,
        ) -> Result<(), Infallible> {
            Ok(())
        }

        async fn wait_ready(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    /// A touch controller replaying scripted frames, half the display's span.
    struct Finger(VecDeque<Option<(u16, u16)>>);

    impl Touch for Finger {
        type Error = Infallible;

        fn descriptor(&self) -> TouchDescriptor {
            TouchDescriptor {
                width: 180,
                height: 180,
                max_points: 1,
            }
        }

        fn read_frame(&mut self) -> Result<TouchFrame, Infallible> {
            let mut frame = TouchFrame::new();
            if let Some(Some((x, y))) = self.0.pop_front() {
                frame.push(TouchPoint {
                    id: 0,
                    x,
                    y,
                    strength: 1,
                });
            }
            Ok(frame)
        }
    }

    fn refresh(screen: &mut Screen<Panel>, service: &UiService) -> bool {
        future::block_on(screen.refresh(service))
    }

    #[test]
    fn the_shown_view_is_drawn_and_then_only_what_changes() {
        let mut screen = Screen::new(Panel::new(DEFAULT_VIEWPORT));
        let service = UiService::new(screen.viewport());
        let session = service.open(&assets()).expect("view opens");
        let full = (DEFAULT_VIEWPORT.0 * DEFAULT_VIEWPORT.1) as u32;

        // Nothing shown: the display is cleared once.
        assert!(refresh(&mut screen, &service));
        assert!(screen_pixels(&screen).iter().all(|pixel| *pixel == BLACK));
        assert!(!refresh(&mut screen, &service));

        session.show(true);
        assert!(refresh(&mut screen, &service));
        assert_eq!(screen_panel(&screen).drawn_pixels(), full * 2);
        assert!(screen_pixels(&screen).iter().any(|pixel| *pixel != BLACK));
        // A settled view draws nothing more.
        assert!(!refresh(&mut screen, &service));

        let before = screen_panel(&screen).drawn_pixels();
        session
            .channel()
            .set("count", crate::ViewValue::I32(7))
            .expect("count");
        service.pump();
        assert!(refresh(&mut screen, &service));
        let redrawn = screen_panel(&screen).drawn_pixels() - before;
        assert!(redrawn > 0 && redrawn < full / 2, "redrew {redrawn} pixels");

        // Hiding blanks the display again.
        session.hide();
        assert!(refresh(&mut screen, &service));
        assert!(screen_pixels(&screen).iter().all(|pixel| *pixel == BLACK));
        assert_eq!(screen_panel(&screen).flushed.len(), 4);
    }

    #[test]
    fn labels_without_a_size_class_are_drawn() {
        let mut screen = Screen::new(Panel::new(DEFAULT_VIEWPORT));
        let service = UiService::new(screen.viewport());
        let session = service.open(&assets()).expect("view opens");
        session.show(true);
        refresh(&mut screen, &service);

        // The "+1" button is lime (#CCFF00); its label (#09090B) has no size class.
        let width = DEFAULT_VIEWPORT.0 as usize;
        let lime = |pixel: &Rgb888| pixel.r() > 180 && pixel.g() > 230 && pixel.b() < 40;
        let pixels = screen_pixels(&screen);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (index, _) in pixels.iter().enumerate().filter(|(_, pixel)| lime(pixel)) {
            let (x, y) = (index % width, index / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        assert!(
            x1 > x0 + 20 && y1 > y0 + 10,
            "no button at {x0},{y0}..{x1},{y1}"
        );
        let label = (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| y * width + x))
            .filter(|index| {
                let pixel = pixels[*index];
                pixel.r() < 60 && pixel.g() < 60 && pixel.b() < 60
            })
            .count();
        assert!(label > 20, "the label drew {label} pixels");
    }

    #[test]
    fn dropping_or_suspending_the_shown_view_takes_it_off_the_display() {
        let service = UiService::new(DEFAULT_VIEWPORT);
        let session = service.open(&assets()).expect("view opens");
        session.show(true);
        assert!(session.is_shown());
        session.suspend();
        assert!(!session.is_shown());
        session.resume(&assets()).expect("resume");
        session.show(false);
        assert!(session.is_shown());
        drop(session);
        assert_eq!(service.shown().view, None);
    }

    #[test]
    fn touches_press_the_shown_view_only_while_it_takes_input() {
        let service = UiService::new(DEFAULT_VIEWPORT);
        let session = service.open(&assets()).expect("view opens");
        let (x, y) = find(&session, "increment");
        session.clear_events();

        // The controller spans half the display, so its points double.
        let (tx, ty) = ((x / 2.0) as u16, (y / 2.0) as u16);
        let mut pointer = Pointer::new(
            Finger(VecDeque::from([
                Some((tx, ty)),
                Some((tx, ty)),
                None,
                Some((tx, ty)),
            ])),
            DEFAULT_VIEWPORT,
        );
        // A contact presses once, however long it lasts.
        let point = pointer.poll().expect("a new contact");
        assert_eq!(pointer.poll(), None);
        assert_eq!(pointer.poll(), None);
        assert_eq!(point, (f32::from(tx) * 2.0, f32::from(ty) * 2.0));

        assert_eq!(service.press_shown(point.0, point.1), None);
        session.show(false);
        assert_eq!(service.press_shown(point.0, point.1), None);
        session.show(true);
        assert_eq!(
            service.press_shown(point.0, point.1).as_deref(),
            Some("increment")
        );
        assert!(pointer.poll().is_some());
    }

    fn screen_panel(screen: &Screen<Panel>) -> &Panel {
        screen.display()
    }

    fn screen_pixels(screen: &Screen<Panel>) -> &[Rgb888] {
        &screen.display().pixels
    }
}
