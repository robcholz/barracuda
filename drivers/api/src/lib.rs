//! Stable Driver and built-in peripheral capability contracts.

#![no_std]

use core::future::Future;

/// A statically selected peripheral Driver.
///
/// Bindings are concrete chip resources supplied by the generated Board HAL.
/// Configuration is generated from the Driver's validated `driver.yml`
/// schema. The returned capability implements a Barracuda semantic peripheral
/// API such as [`display::Display`] or [`indicator::Indicator`].
pub trait PeripheralDriver: Sized + 'static {
    /// Move-only hardware resources consumed by this Driver.
    type Bindings;
    /// Validated, strongly typed initialization configuration.
    type Config;
    /// Semantic capability produced after initialization.
    type Capability;
    /// Initialization failure.
    type Error: core::fmt::Debug;

    /// Initializes the Driver from its concrete bindings and configuration.
    fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> impl Future<Output = Result<Self::Capability, Self::Error>>;
}

/// Stable display capability API.
pub mod display {
    use core::{future::Future, ops::BitOr};

    use embedded_graphics_core::{
        draw_target::DrawTarget,
        geometry::{OriginDimensions, Size},
        primitives::Rectangle,
    };

    /// Pixel storage format reported independently of a Driver's concrete
    /// [`embedded_graphics_core::pixelcolor::PixelColor`] type.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum PixelFormat {
        /// One bit per pixel.
        Binary,
        /// Two bits per pixel grayscale.
        Gray2,
        /// Four bits per pixel grayscale.
        Gray4,
        /// Eight bits per pixel grayscale.
        Gray8,
        /// 16-bit RGB, five red, six green, and five blue bits.
        Rgb565,
        /// 18-bit RGB carried in three bytes.
        Rgb666,
        /// 24-bit RGB carried in three bytes.
        Rgb888,
    }

    /// Physical display technology and its broad presentation behavior.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum DisplayTechnology {
        /// Liquid-crystal panel, including SPI, parallel RGB, and DSI panels.
        Lcd,
        /// Emissive OLED or AMOLED panel.
        Oled,
        /// Reflective electrophoretic panel with explicit refresh cycles.
        Epaper,
        /// Addressable LED pixel matrix.
        LedMatrix,
        /// External video sink such as HDMI rather than a fixed panel.
        VideoOutput,
    }

    /// Runtime properties supported by one concrete display.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct DisplayFeatures(u16);

    impl DisplayFeatures {
        /// Pixels are accumulated before [`Display::flush`] presents them.
        pub const BUFFERED: Self = Self(1 << 0);
        /// A rectangular subregion can be refreshed independently.
        pub const PARTIAL_REFRESH: Self = Self(1 << 1);
        /// Hardware brightness can be controlled.
        pub const BRIGHTNESS: Self = Self(1 << 2);
        /// Hardware orientation can be changed after initialization.
        pub const ORIENTATION: Self = Self(1 << 3);
        /// The display has a sleep state distinct from off.
        pub const SLEEP: Self = Self(1 << 4);
        /// The Driver can report or await controller readiness.
        pub const BUSY: Self = Self(1 << 5);
        /// Pixels can be read back from the display.
        pub const READBACK: Self = Self(1 << 6);

        /// Returns an empty feature set.
        #[must_use]
        pub const fn empty() -> Self {
            Self(0)
        }

        /// Returns whether all `other` features are present.
        #[must_use]
        pub const fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }

        /// Returns the union of two feature sets in constant contexts.
        #[must_use]
        pub const fn union(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }
    }

    impl BitOr for DisplayFeatures {
        type Output = Self;

        fn bitor(self, rhs: Self) -> Self::Output {
            Self(self.0 | rhs.0)
        }
    }

    /// Stable metadata describing one initialized display.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DisplayDescriptor {
        size: Size,
        pixel_format: PixelFormat,
        technology: DisplayTechnology,
        features: DisplayFeatures,
    }

    impl DisplayDescriptor {
        /// Creates a descriptor from its physical drawing size and features.
        #[must_use]
        pub const fn new(
            size: Size,
            pixel_format: PixelFormat,
            technology: DisplayTechnology,
            features: DisplayFeatures,
        ) -> Self {
            Self {
                size,
                pixel_format,
                technology,
                features,
            }
        }

        /// Returns the active drawing dimensions in pixels.
        #[must_use]
        pub const fn size(self) -> Size {
            self.size
        }

        /// Returns the native pixel storage format.
        #[must_use]
        pub const fn pixel_format(self) -> PixelFormat {
            self.pixel_format
        }

        /// Returns the physical display technology.
        #[must_use]
        pub const fn technology(self) -> DisplayTechnology {
            self.technology
        }

        /// Returns optional operations implemented by this display.
        #[must_use]
        pub const fn features(self) -> DisplayFeatures {
            self.features
        }
    }

    /// Display power state requested by a capability consumer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DisplayPower {
        /// Display controller and panel are active.
        On,
        /// Panel retains state in its low-power sleep mode.
        Sleep,
        /// Panel power is disabled when the hardware permits it.
        Off,
    }

    /// Display orientation relative to the Board's native mounting.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum DisplayOrientation {
        /// Native mounting orientation.
        #[default]
        Deg0,
        /// Clockwise quarter turn.
        Deg90,
        /// Half turn.
        Deg180,
        /// Clockwise three-quarter turn.
        Deg270,
    }

    /// Refresh waveform or transfer policy.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum RefreshMode {
        /// Let the Driver select the appropriate policy.
        #[default]
        Automatic,
        /// Force a complete, highest-quality refresh.
        Full,
        /// Prefer a partial refresh when supported.
        Partial,
        /// Prefer the lowest-latency refresh when supported.
        Fast,
    }

    /// One request to present already drawn pixels.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct RefreshRequest {
        region: Option<Rectangle>,
        mode: RefreshMode,
    }

    impl RefreshRequest {
        /// Creates a refresh request. `None` selects the whole display.
        #[must_use]
        pub const fn new(region: Option<Rectangle>, mode: RefreshMode) -> Self {
            Self { region, mode }
        }

        /// Returns the requested region, or `None` for the whole display.
        #[must_use]
        pub const fn region(self) -> Option<Rectangle> {
            self.region
        }

        /// Returns the requested refresh policy.
        #[must_use]
        pub const fn mode(self) -> RefreshMode {
            self.mode
        }
    }

    /// Barracuda's stable semantic interface for a built-in display.
    ///
    /// Pixel drawing intentionally uses the ecosystem-standard [`DrawTarget`]
    /// contract. The extra operations cover presentation and panel lifecycle,
    /// allowing the same API to represent direct LCDs, buffered e-paper,
    /// DSI panels, and external video-output devices.
    pub trait Display: DrawTarget + OriginDimensions {
        /// Failure returned by lifecycle and presentation operations.
        type ControlError: core::fmt::Debug;

        /// Describes this initialized display.
        fn descriptor(&self) -> DisplayDescriptor;

        /// Presents pixels written through [`DrawTarget`].
        fn flush(
            &mut self,
            request: RefreshRequest,
        ) -> impl Future<Output = Result<(), Self::ControlError>>;

        /// Changes panel power state.
        fn set_power(
            &mut self,
            power: DisplayPower,
        ) -> impl Future<Output = Result<(), Self::ControlError>>;

        /// Sets brightness on the portable `0..=255` scale.
        fn set_brightness(
            &mut self,
            brightness: u8,
        ) -> impl Future<Output = Result<(), Self::ControlError>>;

        /// Changes orientation relative to the native Board mounting.
        fn set_orientation(
            &mut self,
            orientation: DisplayOrientation,
        ) -> impl Future<Output = Result<(), Self::ControlError>>;

        /// Waits until the controller can accept the next operation.
        fn wait_ready(&mut self) -> impl Future<Output = Result<(), Self::ControlError>>;
    }

    /// Move-only access to the Board's primary built-in display.
    ///
    /// Generated Board HALs implement this trait when `board.yml` declares a
    /// built-in peripheral whose capability is `display`.
    pub trait BuiltinDisplay {
        /// Concrete, statically dispatched display capability.
        type Display: Display;

        /// Transfers the primary display to its sole consumer once.
        fn take_display(&mut self) -> Option<Self::Display>;
    }
}

/// Stable indicator capability API.
pub mod indicator {
    /// Electrical level that represents the semantic on state.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ActiveLevel {
        /// The indicator is on while its output is low.
        Low,
        /// The indicator is on while its output is high.
        High,
    }

    /// Portable initialization configuration shared by indicator Drivers.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct IndicatorConfig {
        active_level: ActiveLevel,
    }

    impl IndicatorConfig {
        /// Creates configuration for the Board's electrical active level.
        #[must_use]
        pub const fn new(active_level: ActiveLevel) -> Self {
            Self { active_level }
        }

        /// Returns the electrical active level.
        #[must_use]
        pub const fn active_level(self) -> ActiveLevel {
            self.active_level
        }
    }

    /// Semantic on/off indicator independent of electrical active level.
    pub trait Indicator {
        /// Concrete output failure.
        type Error: core::fmt::Debug;

        /// Selects the semantic state.
        fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error>;

        /// Returns the semantic state requested at the hardware latch.
        fn is_enabled(&mut self) -> Result<bool, Self::Error>;

        /// Turns the indicator on.
        fn on(&mut self) -> Result<(), Self::Error> {
            self.set_enabled(true)
        }

        /// Turns the indicator off.
        fn off(&mut self) -> Result<(), Self::Error> {
            self.set_enabled(false)
        }
    }

    /// Move-only access to the Board's primary built-in indicator.
    pub trait BuiltinIndicator {
        /// Concrete, statically dispatched indicator capability.
        type Indicator: Indicator;

        /// Transfers the primary indicator to its sole consumer once.
        fn take_indicator(&mut self) -> Option<Self::Indicator>;
    }
}
