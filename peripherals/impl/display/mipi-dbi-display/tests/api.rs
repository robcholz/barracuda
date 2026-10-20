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

#[allow(clippy::expect_used)]
mod setup {
    use std::{cell::RefCell, rc::Rc, vec::Vec};

    use barracuda_mipi_dbi_display::{
        DigitalBacklight, MipiDbiDisplayConfig, MipiDbiDriverConfig, PanelReset, ST7789,
        initialize_on,
    };
    use barracuda_peripheral::display::{
        Display, DisplayFeatures, DisplayOrientation, PixelFormat,
    };
    use core::convert::Infallible;
    use embedded_graphics_core::geometry::Size;
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType, OutputPin},
    };
    use mipidsi::interface::{Interface, InterfaceKind};

    /// One observable step of panel setup, in order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Event {
        Command(u8),
        Reset(bool),
        Backlight(bool),
    }

    type Log = Rc<RefCell<Vec<Event>>>;

    struct Link(Log);

    impl Interface for Link {
        type Word = u8;
        type Error = Infallible;

        const KIND: InterfaceKind = InterfaceKind::Serial4Line;

        fn send_command(&mut self, command: u8, _args: &[u8]) -> Result<(), Infallible> {
            self.0.borrow_mut().push(Event::Command(command));
            Ok(())
        }

        fn send_pixels<const N: usize>(
            &mut self,
            _pixels: impl IntoIterator<Item = [u8; N]>,
        ) -> Result<(), Infallible> {
            Ok(())
        }

        fn send_repeated_pixel<const N: usize>(
            &mut self,
            _pixel: [u8; N],
            _count: u32,
        ) -> Result<(), Infallible> {
            Ok(())
        }
    }

    /// Output pin that logs its levels as the given event.
    struct Pin(Log, fn(bool) -> Event);

    impl ErrorType for Pin {
        type Error = Infallible;
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().push((self.1)(false));
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().push((self.1)(true));
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn config(reset_active_high: bool) -> MipiDbiDriverConfig<ST7789> {
        MipiDbiDriverConfig::new(
            ST7789,
            MipiDbiDisplayConfig::new(
                Size::new(240, 240),
                PixelFormat::Rgb565,
                DisplayOrientation::Deg0,
            )
            .with_digital_backlight(false)
            .with_reset_active_high(reset_active_high),
        )
    }

    fn backlight(log: &Log) -> Option<DigitalBacklight<Pin>> {
        config(false).digital_backlight(Some(Pin(log.clone(), Event::Backlight)))
    }

    #[test]
    fn without_a_reset_line_the_panel_gets_a_software_reset_first() {
        let log = Log::default();
        let display = initialize_on(
            Link(log.clone()),
            None::<Pin>,
            backlight(&log),
            (),
            Delay,
            config(false),
        )
        .expect("panel");
        let events = log.borrow();
        assert_eq!(events.first(), Some(&Event::Command(0x01)));
        assert!(!events.iter().any(|event| matches!(event, Event::Reset(_))));
        // The active-low backlight is lit last.
        assert_eq!(events.last(), Some(&Event::Backlight(false)));
        assert!(
            display
                .descriptor()
                .features()
                .contains(DisplayFeatures::BRIGHTNESS)
        );
    }

    #[test]
    fn a_reset_line_is_pulsed_with_its_active_level() {
        for (active_high, pulse) in [(false, [false, true]), (true, [true, false])] {
            let log = Log::default();
            initialize_on(
                Link(log.clone()),
                Some(Pin(log.clone(), Event::Reset)),
                backlight(&log),
                (),
                Delay,
                config(active_high),
            )
            .expect("panel");
            let events = log.borrow();
            assert_eq!(
                events[..2],
                [Event::Reset(pulse[0]), Event::Reset(pulse[1])]
            );
            assert_ne!(events.get(2), Some(&Event::Command(0x01)));
        }
    }

    #[test]
    fn the_display_keeps_its_hold_and_works_without_a_backlight() {
        let log = Log::default();
        let hold = Rc::new(());
        let display = initialize_on(
            Link(log.clone()),
            None::<Pin>,
            None::<DigitalBacklight<Pin>>,
            hold.clone(),
            Delay,
            config(false),
        )
        .expect("panel");
        assert_eq!(Rc::strong_count(&hold), 2);
        assert!(
            !display
                .descriptor()
                .features()
                .contains(DisplayFeatures::BRIGHTNESS)
        );
        drop(display);
        assert_eq!(Rc::strong_count(&hold), 1);
    }

    #[test]
    fn digital_backlight_needs_a_declared_polarity() {
        let undeclared = MipiDbiDriverConfig::new(
            ST7789,
            MipiDbiDisplayConfig::new(
                Size::new(240, 240),
                PixelFormat::Rgb565,
                DisplayOrientation::Deg0,
            ),
        );
        let log = Log::default();
        assert!(
            undeclared
                .digital_backlight(Some(Pin(log.clone(), Event::Backlight)))
                .is_none()
        );
        assert!(config(false).digital_backlight(None::<Pin>).is_none());
    }

    #[test]
    fn panel_reset_inverts_an_active_high_line_and_ignores_an_absent_one() {
        let log = Log::default();
        let mut active_high = PanelReset::new(Some(Pin(log.clone(), Event::Reset)), true);
        // `mipidsi` asserts reset by driving low.
        let Ok(()) = active_high.set_low();
        let Ok(()) = active_high.set_high();
        assert_eq!(*log.borrow(), [Event::Reset(true), Event::Reset(false)]);
        let mut absent = PanelReset::<Pin>::new(None, false);
        let Ok(()) = absent.set_low();
        assert_eq!(log.borrow().len(), 2);
    }
}
