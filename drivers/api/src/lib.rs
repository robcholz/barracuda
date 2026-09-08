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
        pixelcolor::Rgb888,
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
        /// Failure returned while converting and drawing portable RGB pixels.
        type RenderError: core::fmt::Debug;

        /// Describes this initialized display.
        fn descriptor(&self) -> DisplayDescriptor;

        /// Draws one row-major RGB888 region using Driver-owned color conversion.
        fn draw_rgb888(
            &mut self,
            area: Rectangle,
            pixels: &[Rgb888],
        ) -> Result<(), Self::RenderError>;

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

/// Stable addressable LED-strip capability API.
pub mod led_strip {
    /// One additive RGB pixel with eight bits per component.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Rgb8 {
        /// Red intensity.
        pub red: u8,
        /// Green intensity.
        pub green: u8,
        /// Blue intensity.
        pub blue: u8,
    }

    impl Rgb8 {
        /// Creates one RGB pixel.
        #[must_use]
        pub const fn new(red: u8, green: u8, blue: u8) -> Self {
            Self { red, green, blue }
        }
    }

    /// Semantic pixel output independent of its wire protocol.
    pub trait LedStrip {
        /// Concrete transfer failure.
        type Error: core::fmt::Debug;

        /// Returns the number of addressable pixels wired on the Board.
        fn len(&self) -> usize;

        /// Returns whether the strip has no addressable pixels.
        fn is_empty(&self) -> bool {
            self.len() == 0
        }

        /// Replaces the strip contents. Missing trailing pixels are turned off.
        fn write(&mut self, pixels: &[Rgb8]) -> Result<(), Self::Error>;

        /// Turns every pixel off.
        fn clear(&mut self) -> Result<(), Self::Error> {
            self.write(&[])
        }
    }

    /// Move-only access to the Board's primary built-in LED strip.
    pub trait BuiltinLedStrip {
        /// Concrete, statically dispatched LED-strip capability.
        type LedStrip: LedStrip;

        /// Transfers the primary LED strip to its sole consumer once.
        fn take_led_strip(&mut self) -> Option<Self::LedStrip>;
    }
}

/// Stable camera capability API.
pub mod camera {
    use core::future::Future;

    /// Error family for a Platform-owned camera data receiver.
    pub trait FrameReceiverErrorType {
        /// Error returned while receiving one sensor frame.
        type Error: core::error::Error;
    }

    /// Narrow data-plane binding used by parallel camera sensor Drivers.
    ///
    /// Sensor control continues to use `embedded-hal` I2C and GPIO traits. A
    /// Platform implements this binding only for the camera receiver mechanism
    /// that `embedded-hal` does not standardize.
    pub trait FrameReceiver: FrameReceiverErrorType {
        /// Receives one complete frame and returns its initialized byte count.
        fn receive<'a>(
            &'a mut self,
            frame: &'a mut [u8],
        ) -> impl Future<Output = Result<usize, Self::Error>> + 'a;
    }

    /// Pixel representation produced by a Camera Driver.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum CameraPixelFormat {
        /// JPEG-compressed byte stream.
        Jpeg,
        /// Packed RGB565 pixels.
        Rgb565,
        /// Packed YUV 4:2:2 pixels.
        Yuv422,
        /// Eight-bit grayscale pixels.
        Grayscale,
    }

    /// Stable description of a configured camera stream.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CameraDescriptor {
        width: u16,
        height: u16,
        pixel_format: CameraPixelFormat,
    }

    impl CameraDescriptor {
        /// Creates a camera stream descriptor.
        #[must_use]
        pub const fn new(width: u16, height: u16, pixel_format: CameraPixelFormat) -> Self {
            Self {
                width,
                height,
                pixel_format,
            }
        }

        /// Returns the active width in pixels.
        #[must_use]
        pub const fn width(self) -> u16 {
            self.width
        }

        /// Returns the active height in pixels.
        #[must_use]
        pub const fn height(self) -> u16 {
            self.height
        }

        /// Returns the produced pixel representation.
        #[must_use]
        pub const fn pixel_format(self) -> CameraPixelFormat {
            self.pixel_format
        }
    }

    /// One captured frame stored in the caller-provided buffer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CapturedFrame {
        bytes_used: usize,
        descriptor: CameraDescriptor,
    }

    impl CapturedFrame {
        /// Creates frame metadata after a successful capture.
        #[must_use]
        pub const fn new(bytes_used: usize, descriptor: CameraDescriptor) -> Self {
            Self {
                bytes_used,
                descriptor,
            }
        }

        /// Returns the initialized prefix length in the capture buffer.
        #[must_use]
        pub const fn bytes_used(self) -> usize {
            self.bytes_used
        }

        /// Returns the format and dimensions of this frame.
        #[must_use]
        pub const fn descriptor(self) -> CameraDescriptor {
            self.descriptor
        }
    }

    /// Semantic still-frame camera capability.
    pub trait Camera {
        /// Concrete control or capture failure.
        type Error: core::fmt::Debug;

        /// Returns the configured stream description.
        fn descriptor(&self) -> CameraDescriptor;

        /// Captures one frame into caller-owned memory.
        fn capture<'a>(
            &'a mut self,
            buffer: &'a mut [u8],
        ) -> impl Future<Output = Result<CapturedFrame, Self::Error>> + 'a;
    }

    /// Move-only access to the Board's primary built-in camera.
    pub trait BuiltinCamera {
        /// Concrete, statically dispatched camera capability.
        type Camera: Camera;

        /// Transfers the primary camera to its sole consumer once.
        fn take_camera(&mut self) -> Option<Self::Camera>;
    }
}

/// Stable audio-codec capability API.
pub mod audio {
    use core::future::Future;

    /// Wire format of a Platform-owned PCM data stream.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PcmFormat {
        /// Audio samples per second, per channel.
        pub sample_rate_hz: u32,
        /// Number of interleaved channels.
        pub channels: u8,
        /// Valid audio bits carried in each word.
        pub bits_per_sample: u8,
        /// External master-clock frequency, or `None` when the codec derives
        /// its clock from BCLK.
        pub master_clock_hz: Option<u32>,
    }

    /// Narrow PCM data-plane binding used by audio-codec Drivers.
    ///
    /// Codec control continues to use `embedded-hal` I2C. A Platform implements
    /// this binding for I2S/PCM transfer, which `embedded-hal` does not
    /// standardize.
    pub trait PcmStream {
        /// Error returned by a PCM transfer.
        type Error: core::error::Error;

        /// Returns the configured wire format.
        fn format(&self) -> PcmFormat;

        /// Writes interleaved signed 16-bit samples.
        fn write<'a>(
            &'a mut self,
            samples: &'a [i16],
        ) -> impl Future<Output = Result<(), Self::Error>> + 'a;

        /// Reads interleaved signed 16-bit samples.
        fn read<'a>(
            &'a mut self,
            samples: &'a mut [i16],
        ) -> impl Future<Output = Result<(), Self::Error>> + 'a;
    }

    /// Stable description of one configured PCM stream.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct AudioDescriptor {
        sample_rate_hz: u32,
        channels: u8,
        bits_per_sample: u8,
    }

    impl AudioDescriptor {
        /// Creates an interleaved PCM stream descriptor.
        #[must_use]
        pub const fn new(sample_rate_hz: u32, channels: u8, bits_per_sample: u8) -> Self {
            Self {
                sample_rate_hz,
                channels,
                bits_per_sample,
            }
        }

        /// Returns the sample rate in hertz.
        #[must_use]
        pub const fn sample_rate_hz(self) -> u32 {
            self.sample_rate_hz
        }

        /// Returns the number of interleaved channels.
        #[must_use]
        pub const fn channels(self) -> u8 {
            self.channels
        }

        /// Returns the valid PCM bits in each I2S word.
        #[must_use]
        pub const fn bits_per_sample(self) -> u8 {
            self.bits_per_sample
        }
    }

    /// Semantic PCM input/output backed by an external audio codec.
    pub trait AudioCodec {
        /// Concrete register or stream failure.
        type Error: core::fmt::Debug;

        /// Returns the configured PCM stream description.
        fn descriptor(&self) -> AudioDescriptor;

        /// Sets output volume on a portable `0..=255` scale.
        fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error>;

        /// Writes interleaved signed PCM samples.
        fn write<'a>(
            &'a mut self,
            samples: &'a [i16],
        ) -> impl Future<Output = Result<(), Self::Error>> + 'a;

        /// Reads interleaved signed PCM samples.
        fn read<'a>(
            &'a mut self,
            samples: &'a mut [i16],
        ) -> impl Future<Output = Result<(), Self::Error>> + 'a;
    }

    /// Move-only access to the Board's primary built-in audio codec.
    pub trait BuiltinAudioCodec {
        /// Concrete, statically dispatched codec capability.
        type AudioCodec: AudioCodec;

        /// Transfers the primary codec to its sole consumer once.
        fn take_audio_codec(&mut self) -> Option<Self::AudioCodec>;
    }
}
