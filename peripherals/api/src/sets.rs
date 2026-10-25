//! Device sets: one statically dispatched type over a Board's devices of one
//! kind.
//!
//! A Board may declare several devices of a kind. Each kind's trait takes a
//! device by [`Instance`](crate::Instance) and returns one associated type, so
//! the generated Board HAL names the devices with one of the macros here: an
//! enum with a variant per concrete device that implements the kind's trait by
//! forwarding each call to the device it holds, and one error enum per
//! associated error type with a variant per device. A Board with one device of
//! a kind uses that device's concrete type and no set.
//!
//! Every kind's macro has the same form, `<kind>_set! { pub enum Name, Error
//! { Variant(Type), ... } }`; [`display_set!`] alone names two error types
//! because [`Display`](crate::display::Display) has two. A new kind adds its
//! macro here, built from the helpers below.

/// Declares a set's device enum.
#[doc(hidden)]
#[macro_export]
macro_rules! __peripheral_set_enum {
    ($vis:vis $name:ident, $kind:literal, [$($variant:ident($device:ty)),+]) => {
        #[doc = concat!("One of the Board's ", $kind, " devices.")]
        #[allow(clippy::large_enum_variant)]
        $vis enum $name {
            $(
                #[doc = concat!("The `", stringify!($variant), "` ", $kind, ".")]
                $variant($device),
            )+
        }
    };
}

/// Declares a set's error enum for one associated error type of the kind's
/// trait.
#[doc(hidden)]
#[macro_export]
macro_rules! __peripheral_set_error {
    (
        $vis:vis $error:ident, $kind:literal, <$trait:path>::$associated:ident,
        [$($variant:ident($device:ty)),+]
    ) => {
        #[doc = concat!("A failure of one of the Board's ", $kind, " devices.")]
        #[derive(Debug)]
        $vis enum $error {
            $(
                #[doc = concat!("The `", stringify!($variant), "` ", $kind, " failed.")]
                $variant(<$device as $trait>::$associated),
            )+
        }
    };
}

/// Forwards one call to the device a set holds: `plain` returns the device's
/// result, `map Error` wraps its error in the set's error variant.
#[doc(hidden)]
#[macro_export]
macro_rules! __peripheral_set_forward {
    (plain $set:expr, $name:ident [$($variant:ident),+] |$device:ident| $call:expr) => {
        match $set {
            $($name::$variant($device) => $call,)+
        }
    };
    (map $error:ident: $set:expr, $name:ident [$($variant:ident),+] |$device:ident| $call:expr) => {
        match $set {
            $($name::$variant($device) => ($call).map_err($error::$variant),)+
        }
    };
}

/// Declares one Board's displays as a single statically dispatched type.
///
/// Generated Board HALs use this when a Board declares several displays: the
/// enum holds whichever concrete display a claimed instance names, and
/// implements [`display::Display`](crate::display::Display) by forwarding to
/// it. Drawing through
/// [`DrawTarget`](embedded_graphics_core::draw_target::DrawTarget) takes
/// portable RGB565 pixels and draws them through
/// [`display::Display::draw_rgb565`](crate::display::Display::draw_rgb565), so
/// displays with different native color types share one type.
///
/// ```ignore
/// barracuda_peripheral::display_set! {
///     pub enum BoardDisplay, BoardDisplayControlError, BoardDisplayRenderError {
///         Front(FrontPeripheral),
///         BackEink(BackEinkPeripheral),
///     }
/// }
/// ```
#[macro_export]
macro_rules! display_set {
    (
        $vis:vis enum $name:ident, $control:ident, $render:ident {
            $($variant:ident($display:ty)),+ $(,)?
        }
    ) => {
        $crate::__peripheral_set_enum!($vis $name, "display", [$($variant($display)),+]);
        $crate::__peripheral_set_error!(
            $vis $control, "display", <$crate::display::Display>::ControlError,
            [$($variant($display)),+]
        );
        $crate::__peripheral_set_error!(
            $vis $render, "display", <$crate::display::Display>::RenderError,
            [$($variant($display)),+]
        );

        impl $crate::__private::embedded_graphics_core::geometry::OriginDimensions for $name {
            fn size(&self) -> $crate::__private::embedded_graphics_core::geometry::Size {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |display| {
                    $crate::__private::embedded_graphics_core::geometry::OriginDimensions::size(
                        display,
                    )
                })
            }
        }

        impl $crate::__private::embedded_graphics_core::draw_target::DrawTarget for $name {
            type Color = $crate::__private::embedded_graphics_core::pixelcolor::Rgb565;
            type Error = $render;

            fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
            where
                I: IntoIterator<
                    Item = $crate::__private::embedded_graphics_core::Pixel<Self::Color>,
                >,
            {
                use $crate::__private::embedded_graphics_core::{
                    geometry::Size, primitives::Rectangle, Pixel,
                };
                for Pixel(point, color) in pixels {
                    let area = Rectangle::new(point, Size::new(1, 1));
                    $crate::__peripheral_set_forward!(map $render: &mut *self, $name [$($variant),+] |display| {
                        $crate::display::Display::draw_rgb565(display, area, &[color])
                    })?;
                }
                Ok(())
            }
        }

        impl $crate::display::Display for $name {
            type ControlError = $control;
            type RenderError = $render;

            fn descriptor(&self) -> $crate::display::DisplayDescriptor {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |display| {
                    $crate::display::Display::descriptor(display)
                })
            }

            fn draw_rgb565(
                &mut self,
                area: $crate::__private::embedded_graphics_core::primitives::Rectangle,
                pixels: &[$crate::__private::embedded_graphics_core::pixelcolor::Rgb565],
            ) -> Result<(), Self::RenderError> {
                $crate::__peripheral_set_forward!(map $render: self, $name [$($variant),+] |display| {
                    $crate::display::Display::draw_rgb565(display, area, pixels)
                })
            }

            async fn flush(
                &mut self,
                request: $crate::display::RefreshRequest,
            ) -> Result<(), Self::ControlError> {
                $crate::__peripheral_set_forward!(map $control: self, $name [$($variant),+] |display| {
                    $crate::display::Display::flush(display, request).await
                })
            }

            async fn set_power(
                &mut self,
                power: $crate::display::DisplayPower,
            ) -> Result<(), Self::ControlError> {
                $crate::__peripheral_set_forward!(map $control: self, $name [$($variant),+] |display| {
                    $crate::display::Display::set_power(display, power).await
                })
            }

            async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
                $crate::__peripheral_set_forward!(map $control: self, $name [$($variant),+] |display| {
                    $crate::display::Display::set_brightness(display, brightness).await
                })
            }

            async fn set_orientation(
                &mut self,
                orientation: $crate::display::DisplayOrientation,
            ) -> Result<(), Self::ControlError> {
                $crate::__peripheral_set_forward!(map $control: self, $name [$($variant),+] |display| {
                    $crate::display::Display::set_orientation(display, orientation).await
                })
            }

            async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
                $crate::__peripheral_set_forward!(map $control: self, $name [$($variant),+] |display| {
                    $crate::display::Display::wait_ready(display).await
                })
            }
        }
    };
}

/// Declares one Board's power monitors as a single statically dispatched
/// [`power::PowerMonitor`](crate::power::PowerMonitor).
#[macro_export]
macro_rules! power_monitor_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "power monitor", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "power monitor", <$crate::power::PowerMonitor>::Error,
            [$($variant($device)),+]
        );

        impl $crate::power::PowerMonitor for $name {
            type Error = $error;

            fn measure(&mut self) -> Result<$crate::power::PowerMeasurement, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::power::PowerMonitor::measure(device)
                })
            }

            fn battery(&mut self) -> Result<Option<$crate::power::BatteryState>, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::power::PowerMonitor::battery(device)
                })
            }
        }
    };
}

/// Declares one Board's real-time clocks as a single statically dispatched
/// [`real_time_clock::RealTimeClock`](crate::real_time_clock::RealTimeClock).
#[macro_export]
macro_rules! real_time_clock_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "real-time clock", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "real-time clock", <$crate::real_time_clock::RealTimeClock>::Error,
            [$($variant($device)),+]
        );

        impl $crate::real_time_clock::RealTimeClock for $name {
            type Error = $error;

            fn lost_power(&mut self) -> Result<bool, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::real_time_clock::RealTimeClock::lost_power(device)
                })
            }

            fn read_datetime(
                &mut self,
            ) -> Result<$crate::real_time_clock::DateTime, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::real_time_clock::RealTimeClock::read_datetime(device)
                })
            }

            fn set_datetime(
                &mut self,
                datetime: $crate::real_time_clock::DateTime,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::real_time_clock::RealTimeClock::set_datetime(device, datetime)
                })
            }
        }
    };
}

/// Declares one Board's touch inputs as a single statically dispatched
/// [`touch::Touch`](crate::touch::Touch).
#[macro_export]
macro_rules! touch_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "touch input", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "touch input", <$crate::touch::Touch>::Error,
            [$($variant($device)),+]
        );

        impl $crate::touch::Touch for $name {
            type Error = $error;

            fn descriptor(&self) -> $crate::touch::TouchDescriptor {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::touch::Touch::descriptor(device)
                })
            }

            fn read_frame(&mut self) -> Result<$crate::touch::TouchFrame, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::touch::Touch::read_frame(device)
                })
            }
        }
    };
}

/// Declares one Board's button groups as a single statically dispatched
/// [`buttons::Buttons`](crate::buttons::Buttons).
#[macro_export]
macro_rules! buttons_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "buttons", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "buttons", <$crate::buttons::Buttons>::Error,
            [$($variant($device)),+]
        );

        impl $crate::buttons::Buttons for $name {
            type Error = $error;

            fn read(&mut self) -> Result<$crate::buttons::ButtonSnapshot, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buttons::Buttons::read(device)
                })
            }

            fn poll_event(
                &mut self,
            ) -> Result<Option<$crate::buttons::ButtonEvent>, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buttons::Buttons::poll_event(device)
                })
            }
        }
    };
}

/// Declares one Board's indicators as a single statically dispatched
/// [`indicator::Indicator`](crate::indicator::Indicator).
#[macro_export]
macro_rules! indicator_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "indicator", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "indicator", <$crate::indicator::Indicator>::Error,
            [$($variant($device)),+]
        );

        impl $crate::indicator::Indicator for $name {
            type Error = $error;

            fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::indicator::Indicator::set_enabled(device, enabled)
                })
            }

            fn is_enabled(&mut self) -> Result<bool, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::indicator::Indicator::is_enabled(device)
                })
            }

            fn on(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::indicator::Indicator::on(device)
                })
            }

            fn off(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::indicator::Indicator::off(device)
                })
            }
        }
    };
}

/// Declares one Board's LED strips as a single statically dispatched
/// [`led_strip::LedStrip`](crate::led_strip::LedStrip).
#[macro_export]
macro_rules! led_strip_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "LED strip", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "LED strip", <$crate::led_strip::LedStrip>::Error,
            [$($variant($device)),+]
        );

        impl $crate::led_strip::LedStrip for $name {
            type Error = $error;

            fn len(&self) -> usize {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::led_strip::LedStrip::len(device)
                })
            }

            fn is_empty(&self) -> bool {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::led_strip::LedStrip::is_empty(device)
                })
            }

            fn write(&mut self, pixels: &[$crate::led_strip::Rgb8]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::led_strip::LedStrip::write(device, pixels)
                })
            }

            fn clear(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::led_strip::LedStrip::clear(device)
                })
            }
        }
    };
}

/// Declares one Board's buzzers as a single statically dispatched
/// [`buzzer::Buzzer`](crate::buzzer::Buzzer).
#[macro_export]
macro_rules! buzzer_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "buzzer", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "buzzer", <$crate::buzzer::Buzzer>::Error,
            [$($variant($device)),+]
        );

        impl $crate::buzzer::Buzzer for $name {
            type Error = $error;

            fn frequency_range(&self) -> (u32, u32) {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::buzzer::Buzzer::frequency_range(device)
                })
            }

            fn start(
                &mut self,
                frequency_hz: u32,
                volume: $crate::buzzer::Volume,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buzzer::Buzzer::start(device, frequency_hz, volume)
                })
            }

            fn stop(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buzzer::Buzzer::stop(device)
                })
            }

            async fn play_note(
                &mut self,
                note: $crate::buzzer::Note,
                volume: $crate::buzzer::Volume,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buzzer::Buzzer::play_note(device, note, volume).await
                })
            }

            async fn play(
                &mut self,
                melody: &[$crate::buzzer::Note],
                volume: $crate::buzzer::Volume,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::buzzer::Buzzer::play(device, melody, volume).await
                })
            }
        }
    };
}

/// Declares one Board's vibration motors as a single statically dispatched
/// [`vibration::Vibration`](crate::vibration::Vibration).
#[macro_export]
macro_rules! vibration_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "vibration motor", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "vibration motor", <$crate::vibration::Vibration>::Error,
            [$($variant($device)),+]
        );

        impl $crate::vibration::Vibration for $name {
            type Error = $error;

            fn has_variable_strength(&self) -> bool {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::vibration::Vibration::has_variable_strength(device)
                })
            }

            fn set(&mut self, strength: $crate::vibration::Strength) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::vibration::Vibration::set(device, strength)
                })
            }

            async fn pulse(
                &mut self,
                pattern: &[$crate::vibration::Pulse],
                strength: $crate::vibration::Strength,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::vibration::Vibration::pulse(device, pattern, strength).await
                })
            }
        }
    };
}

/// Declares one Board's infrared devices as a single statically dispatched
/// [`infrared::Infrared`](crate::infrared::Infrared).
#[macro_export]
macro_rules! infrared_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "infrared device", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "infrared device", <$crate::infrared::Infrared>::Error,
            [$($variant($device)),+]
        );

        impl $crate::infrared::Infrared for $name {
            type Error = $error;

            fn can_receive(&self) -> bool {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::infrared::Infrared::can_receive(device)
                })
            }

            async fn send_nec(
                &mut self,
                command: $crate::infrared::NecCommand,
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::infrared::Infrared::send_nec(device, command).await
                })
            }

            async fn send_raw(
                &mut self,
                carrier: $crate::infrared::Carrier,
                timings_us: &[u16],
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::infrared::Infrared::send_raw(device, carrier, timings_us).await
                })
            }

            async fn receive(
                &mut self,
                timings_us: &mut [u16],
            ) -> Result<$crate::infrared::Received, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::infrared::Infrared::receive(device, timings_us).await
                })
            }
        }
    };
}

/// Declares one Board's cameras as a single statically dispatched
/// [`camera::Camera`](crate::camera::Camera).
#[macro_export]
macro_rules! camera_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "camera", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "camera", <$crate::camera::Camera>::Error,
            [$($variant($device)),+]
        );

        impl $crate::camera::Camera for $name {
            type Error = $error;

            fn descriptor(&self) -> $crate::camera::CameraDescriptor {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::camera::Camera::descriptor(device)
                })
            }

            async fn capture<'a>(
                &'a mut self,
                buffer: &'a mut [u8],
            ) -> Result<$crate::camera::CapturedFrame, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::camera::Camera::capture(device, buffer).await
                })
            }
        }
    };
}

/// Declares one Board's audio codecs as a single statically dispatched
/// [`audio::AudioCodec`](crate::audio::AudioCodec).
#[macro_export]
macro_rules! audio_codec_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "audio codec", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "audio codec", <$crate::audio::AudioCodec>::Error,
            [$($variant($device)),+]
        );

        impl $crate::audio::AudioCodec for $name {
            type Error = $error;

            fn descriptor(&self) -> $crate::audio::AudioDescriptor {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::audio::AudioCodec::descriptor(device)
                })
            }

            fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::audio::AudioCodec::set_output_volume(device, volume)
                })
            }

            async fn write<'a>(&'a mut self, samples: &'a [i16]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::audio::AudioCodec::write(device, samples).await
                })
            }

            async fn read<'a>(&'a mut self, samples: &'a mut [i16]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::audio::AudioCodec::read(device, samples).await
                })
            }
        }
    };
}

/// Declares one Board's keyboards as a single statically dispatched
/// [`keyboard::Keyboard`](crate::keyboard::Keyboard).
#[macro_export]
macro_rules! keyboard_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "keyboard", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "keyboard", <$crate::keyboard::Keyboard>::Error,
            [$($variant($device)),+]
        );

        impl $crate::keyboard::Keyboard for $name {
            type Error = $error;

            fn poll_event(&mut self) -> Result<Option<$crate::keyboard::KeyEvent>, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::keyboard::Keyboard::poll_event(device)
                })
            }
        }
    };
}

/// Declares one Board's rotary encoders as a single statically dispatched
/// [`rotary_encoder::RotaryEncoder`](crate::rotary_encoder::RotaryEncoder).
#[macro_export]
macro_rules! rotary_encoder_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "rotary encoder", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "rotary encoder", <$crate::rotary_encoder::RotaryEncoder>::Error,
            [$($variant($device)),+]
        );

        impl $crate::rotary_encoder::RotaryEncoder for $name {
            type Error = $error;

            fn position(&mut self) -> Result<i64, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::rotary_encoder::RotaryEncoder::position(device)
                })
            }

            fn delta(&mut self) -> Result<i64, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::rotary_encoder::RotaryEncoder::delta(device)
                })
            }

            fn reset(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::rotary_encoder::RotaryEncoder::reset(device)
                })
            }
        }
    };
}

/// Declares one Board's IMUs as a single statically dispatched
/// [`imu::Imu`](crate::imu::Imu).
#[macro_export]
macro_rules! imu_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "IMU", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "IMU", <$crate::imu::Imu>::Error,
            [$($variant($device)),+]
        );

        impl $crate::imu::Imu for $name {
            type Error = $error;

            fn descriptor(&self) -> $crate::imu::ImuDescriptor {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::imu::Imu::descriptor(device)
                })
            }

            async fn read_sample(&mut self) -> Result<$crate::imu::ImuSample, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::imu::Imu::read_sample(device).await
                })
            }
        }
    };
}

/// Declares one Board's environment sensors as a single statically dispatched
/// [`environment::EnvironmentSensor`](crate::environment::EnvironmentSensor).
#[macro_export]
macro_rules! environment_sensor_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "environment sensor", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "environment sensor", <$crate::environment::EnvironmentSensor>::Error,
            [$($variant($device)),+]
        );

        impl $crate::environment::EnvironmentSensor for $name {
            type Error = $error;

            fn quantities(&self) -> $crate::environment::Quantities {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::environment::EnvironmentSensor::quantities(device)
                })
            }

            async fn read(&mut self) -> Result<$crate::environment::EnvironmentReading, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::environment::EnvironmentSensor::read(device).await
                })
            }
        }
    };
}

/// Declares one Board's EEPROMs as a single statically dispatched
/// [`eeprom::Eeprom`](crate::eeprom::Eeprom).
#[macro_export]
macro_rules! eeprom_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "EEPROM", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "EEPROM", <$crate::eeprom::Eeprom>::Error,
            [$($variant($device)),+]
        );

        impl $crate::eeprom::Eeprom for $name {
            type Error = $error;

            fn geometry(&self) -> $crate::eeprom::EepromGeometry {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::eeprom::Eeprom::geometry(device)
                })
            }

            async fn read<'a>(
                &'a mut self,
                offset: u32,
                buffer: &'a mut [u8],
            ) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::eeprom::Eeprom::read(device, offset, buffer).await
                })
            }

            async fn write<'a>(&'a mut self, offset: u32, data: &'a [u8]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::eeprom::Eeprom::write(device, offset, data).await
                })
            }
        }
    };
}

/// Declares one Board's secure elements as a single statically dispatched
/// [`secure_element::SecureElement`](crate::secure_element::SecureElement).
#[macro_export]
macro_rules! secure_element_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "secure element", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "secure element", <$crate::secure_element::SecureElement>::Error,
            [$($variant($device)),+]
        );

        impl $crate::secure_element::SecureElement for $name {
            type Error = $error;

            async fn serial_number(
                &mut self,
            ) -> Result<$crate::secure_element::SerialNumber, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::secure_element::SecureElement::serial_number(device).await
                })
            }

            async fn fill_random<'a>(&'a mut self, buffer: &'a mut [u8]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::secure_element::SecureElement::fill_random(device, buffer).await
                })
            }

            async fn public_key(
                &mut self,
                slot: $crate::secure_element::KeySlot,
            ) -> Result<$crate::secure_element::P256PublicKey, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::secure_element::SecureElement::public_key(device, slot).await
                })
            }

            async fn sign_digest<'a>(
                &'a mut self,
                slot: $crate::secure_element::KeySlot,
                digest: &'a [u8; 32],
            ) -> Result<$crate::secure_element::P256Signature, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::secure_element::SecureElement::sign_digest(device, slot, digest).await
                })
            }
        }
    };
}

/// Declares one Board's NFC readers as a single statically dispatched
/// [`nfc_reader::NfcReader`](crate::nfc_reader::NfcReader).
#[macro_export]
macro_rules! nfc_reader_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "NFC reader", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "NFC reader", <$crate::nfc_reader::NfcReader>::Error,
            [$($variant($device)),+]
        );

        impl $crate::nfc_reader::NfcReader for $name {
            type Error = $error;

            async fn poll(&mut self) -> Result<Option<$crate::nfc_reader::Card>, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::nfc_reader::NfcReader::poll(device).await
                })
            }

            async fn read_block(
                &mut self,
                block: u8,
            ) -> Result<[u8; $crate::nfc_reader::BLOCK_BYTES], Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::nfc_reader::NfcReader::read_block(device, block).await
                })
            }
        }
    };
}

/// Declares one Board's LoRa radios as a single statically dispatched
/// [`lora::Lora`](crate::lora::Lora).
#[macro_export]
macro_rules! lora_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "LoRa radio", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "LoRa radio", <$crate::lora::Lora>::Error,
            [$($variant($device)),+]
        );

        impl $crate::lora::Lora for $name {
            type Error = $error;

            fn limits(&self) -> $crate::lora::RadioLimits {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::lora::Lora::limits(device)
                })
            }

            async fn configure(&mut self, config: $crate::lora::LoraConfig) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::lora::Lora::configure(device, config).await
                })
            }

            async fn transmit(&mut self, payload: &[u8]) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::lora::Lora::transmit(device, payload).await
                })
            }

            async fn receive(
                &mut self,
                buffer: &mut [u8],
            ) -> Result<$crate::lora::Received, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::lora::Lora::receive(device, buffer).await
                })
            }
        }
    };
}

/// Declares one Board's CAN controllers as a single statically dispatched
/// [`can::Can`](crate::can::Can).
#[macro_export]
macro_rules! can_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "CAN controller", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "CAN controller", <$crate::can::Can>::Error,
            [$($variant($device)),+]
        );

        impl $crate::can::Can for $name {
            type Error = $error;

            fn supports_bitrate(&self, bitrate: u32) -> bool {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::can::Can::supports_bitrate(device, bitrate)
                })
            }

            fn start(&mut self, bitrate: u32) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::can::Can::start(device, bitrate)
                })
            }

            fn stop(&mut self) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::can::Can::stop(device)
                })
            }

            fn bitrate(&self) -> Option<u32> {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::can::Can::bitrate(device)
                })
            }

            fn state(&self) -> $crate::can::BusState {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::can::Can::state(device)
                })
            }

            async fn transmit(&mut self, frame: &$crate::can::Frame) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::can::Can::transmit(device, frame).await
                })
            }

            async fn receive(&mut self) -> Result<$crate::can::Frame, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::can::Can::receive(device).await
                })
            }
        }
    };
}

/// Declares one Board's digital I/O devices as a single statically dispatched
/// [`digital_io::DigitalIo`](crate::digital_io::DigitalIo).
#[macro_export]
macro_rules! digital_io_set {
    ($vis:vis enum $name:ident, $error:ident { $($variant:ident($device:ty)),+ $(,)? }) => {
        $crate::__peripheral_set_enum!($vis $name, "digital I/O device", [$($variant($device)),+]);
        $crate::__peripheral_set_error!(
            $vis $error, "digital I/O device", <$crate::digital_io::DigitalIo>::Error,
            [$($variant($device)),+]
        );

        impl $crate::digital_io::DigitalIo for $name {
            type Error = $error;

            fn output_count(&self) -> usize {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::digital_io::DigitalIo::output_count(device)
                })
            }

            fn input_count(&self) -> usize {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::digital_io::DigitalIo::input_count(device)
                })
            }

            fn set_output(&mut self, index: usize, on: bool) -> Result<(), Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::digital_io::DigitalIo::set_output(device, index, on)
                })
            }

            fn outputs(&self) -> $crate::digital_io::Levels {
                $crate::__peripheral_set_forward!(plain self, $name [$($variant),+] |device| {
                    $crate::digital_io::DigitalIo::outputs(device)
                })
            }

            fn read_inputs(&mut self) -> Result<$crate::digital_io::Levels, Self::Error> {
                $crate::__peripheral_set_forward!(map $error: self, $name [$($variant),+] |device| {
                    $crate::digital_io::DigitalIo::read_inputs(device)
                })
            }
        }
    };
}
