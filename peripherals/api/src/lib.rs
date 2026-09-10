//! Stable implementation and peripheral contracts.

#![no_std]

use core::future::Future;

/// A statically selected chip or protocol implementation of one peripheral API.
///
/// Bindings are concrete chip resources supplied by the generated Board HAL.
/// Configuration is generated from the implementation's validated `peripheral.yml`
/// schema. The returned peripheral implements a Barracuda semantic API such as
/// [`display::Display`] or [`indicator::Indicator`].
pub trait PeripheralImplementation: Sized + 'static {
    /// Move-only hardware resources consumed by this implementation.
    type Bindings;
    /// Validated, strongly typed initialization configuration.
    type Config;
    /// Initialized peripheral produced by this implementation.
    type Peripheral;
    /// Initialization failure.
    type Error: core::fmt::Debug;

    /// Initializes the implementation from its concrete bindings and configuration.
    fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> impl Future<Output = Result<Self::Peripheral, Self::Error>>;
}

/// Stable removable-filesystem peripheral API.
pub mod removable_storage {
    use core::future::Future;

    use barracuda_vfs::Backend;

    /// Runtime availability of the medium in one Board-attached slot.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum RemovableStorageStatus {
        /// The slot exists but currently has no mounted filesystem.
        #[default]
        Absent,
        /// One filesystem is mounted from the current medium generation.
        Mounted {
            /// Identity of this insertion, changed whenever the medium changes.
            generation: u32,
        },
    }

    /// One filesystem lifecycle transition reported by a removable slot.
    pub enum RemovableStorageEvent {
        /// A newly inserted and recognized filesystem is ready to mount.
        Mounted {
            /// Identity of this insertion.
            generation: u32,
            /// Type-erased filesystem backend owned by the VFS mount.
            filesystem: Backend,
        },
        /// The current medium disappeared or stopped responding.
        Removed {
            /// Identity of the insertion that disappeared.
            generation: u32,
        },
    }

    /// One Board-attached slot that produces mountable filesystems.
    pub trait RemovableStorage: Send + 'static {
        /// Stable Board peripheral instance name used beneath `/removable`.
        fn slot_id(&self) -> &'static str;

        /// Returns the current runtime state without waiting.
        fn status(&self) -> RemovableStorageStatus;

        /// Waits until the slot produces its next filesystem lifecycle event.
        fn next_event(&mut self) -> impl Future<Output = RemovableStorageEvent>;
    }

    /// Move-only access to the Board's optional removable filesystem slot.
    pub trait RemovableStoragePeripheral {
        /// Concrete, statically dispatched slot implementation.
        type RemovableStorage: RemovableStorage;

        /// Transfers the removable slot to its sole lifecycle owner once.
        fn take_removable_storage(&mut self) -> Option<Self::RemovableStorage>;
    }
}

/// Stable electrical-power monitoring peripheral API.
pub mod power {
    /// One coherent voltage, current, and power sample in integer SI subunits.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PowerMeasurement {
        /// Positive monitored bus voltage in microvolts.
        pub bus_microvolts: u64,
        /// Signed shunt voltage in nanovolts.
        pub shunt_nanovolts: i64,
        /// Signed current in microamps; positive follows the monitor's IN+ to IN- direction.
        pub current_microamps: i64,
        /// Signed calculated power in microwatts.
        pub power_microwatts: i64,
    }

    /// Semantic voltage, current, and power monitor peripheral.
    pub trait PowerMonitor {
        /// Concrete register or sampling failure.
        type Error: core::fmt::Debug;

        /// Reads one coherent electrical measurement.
        fn measure(&mut self) -> Result<PowerMeasurement, Self::Error>;
    }

    /// Move-only access to the Board's primary attached power monitor.
    pub trait PowerMonitorPeripheral {
        /// Concrete, statically dispatched power-monitor peripheral.
        type PowerMonitor: PowerMonitor;

        /// Transfers the primary power monitor to its sole consumer once.
        fn take_power_monitor(&mut self) -> Option<Self::PowerMonitor>;
    }
}

/// Stable real-time clock peripheral API.
pub mod real_time_clock {
    /// One Gregorian date and 24-hour wall-clock time.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DateTime {
        /// Full year.
        pub year: u16,
        /// Month in the range 1 through 12.
        pub month: u8,
        /// Day of month in the range 1 through 31.
        pub day: u8,
        /// Day of week in the range 0 through 6.
        pub weekday: u8,
        /// Hour in the range 0 through 23.
        pub hour: u8,
        /// Minute in the range 0 through 59.
        pub minute: u8,
        /// Second in the range 0 through 59.
        pub second: u8,
    }

    /// Semantic retained wall-clock peripheral.
    pub trait RealTimeClock {
        /// Concrete register or validation failure.
        type Error: core::fmt::Debug;

        /// Reports whether supply loss may have invalidated the retained time.
        fn lost_power(&mut self) -> Result<bool, Self::Error>;

        /// Reads one coherent calendar and clock value.
        fn read_datetime(&mut self) -> Result<DateTime, Self::Error>;

        /// Replaces the retained calendar and clock value.
        fn set_datetime(&mut self, datetime: DateTime) -> Result<(), Self::Error>;
    }

    /// Optional real-time clock position in a Board peripheral set.
    pub trait RealTimeClockPeripheral {
        /// Concrete, statically dispatched clock implementation.
        type RealTimeClock: RealTimeClock;

        /// Transfers the clock to its sole consumer when one is present.
        fn take_real_time_clock(&mut self) -> Option<Self::RealTimeClock>;
    }
}

/// Stable touch-input peripheral API.
pub mod touch {
    /// Maximum contacts represented by the portable touch frame.
    pub const MAX_TOUCH_POINTS: usize = 10;

    /// One reported contact in display coordinates.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct TouchPoint {
        /// Controller-assigned contact identifier when available.
        pub id: u8,
        /// Horizontal coordinate.
        pub x: u16,
        /// Vertical coordinate.
        pub y: u16,
        /// Contact-area or pressure estimate.
        pub strength: u16,
    }

    /// One multi-touch sample.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TouchFrame {
        points: [Option<TouchPoint>; MAX_TOUCH_POINTS],
        count: u8,
    }

    impl TouchFrame {
        /// Creates an empty touch frame.
        #[must_use]
        pub const fn new() -> Self {
            Self {
                points: [None; MAX_TOUCH_POINTS],
                count: 0,
            }
        }

        /// Returns the number of active contacts.
        #[must_use]
        pub const fn len(&self) -> usize {
            self.count as usize
        }

        /// Returns whether no contact is active.
        #[must_use]
        pub const fn is_empty(&self) -> bool {
            self.count == 0
        }

        /// Returns one contact by dense frame index.
        #[must_use]
        pub const fn get(&self, index: usize) -> Option<TouchPoint> {
            if index < self.count as usize {
                self.points[index]
            } else {
                None
            }
        }

        /// Appends one contact and returns whether it fit in the portable frame.
        pub fn push(&mut self, point: TouchPoint) -> bool {
            let index = self.count as usize;
            if index >= MAX_TOUCH_POINTS {
                return false;
            }
            self.points[index] = Some(point);
            self.count = self.count.saturating_add(1);
            true
        }
    }

    impl Default for TouchFrame {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Geometry and contact capacity reported by a touch controller.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TouchDescriptor {
        /// Horizontal coordinate span.
        pub width: u16,
        /// Vertical coordinate span.
        pub height: u16,
        /// Maximum simultaneously reported contacts.
        pub max_points: u8,
    }

    /// Semantic multi-touch input peripheral.
    pub trait Touch {
        /// Concrete register or report failure.
        type Error: core::fmt::Debug;

        /// Returns the initialized touch geometry.
        fn descriptor(&self) -> TouchDescriptor;

        /// Reads the latest contact frame.
        fn read_frame(&mut self) -> Result<TouchFrame, Self::Error>;
    }

    /// Optional touch-input position in a Board peripheral set.
    pub trait TouchPeripheral {
        /// Concrete, statically dispatched touch implementation.
        type Touch: Touch;

        /// Transfers the touch input to its sole consumer when one is present.
        fn take_touch(&mut self) -> Option<Self::Touch>;
    }
}

/// Stable display peripheral API.
pub mod display {
    use core::{future::Future, ops::BitOr};

    use embedded_graphics_core::{
        draw_target::DrawTarget,
        geometry::{OriginDimensions, Size},
        pixelcolor::Rgb888,
        primitives::Rectangle,
    };

    /// Horizontal and vertical video timing for one DSI DPI panel.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DsiVideoTiming {
        /// Active horizontal pixels.
        pub width: u16,
        /// Active vertical pixels.
        pub height: u16,
        /// Horizontal back porch in pixel clocks.
        pub hsync_back_porch: u16,
        /// Horizontal sync pulse in pixel clocks.
        pub hsync_pulse_width: u16,
        /// Horizontal front porch in pixel clocks.
        pub hsync_front_porch: u16,
        /// Vertical back porch in lines.
        pub vsync_back_porch: u16,
        /// Vertical sync pulse in lines.
        pub vsync_pulse_width: u16,
        /// Vertical front porch in lines.
        pub vsync_front_porch: u16,
    }

    /// DSI link and DPI timing chosen by a panel implementation.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DsiPanelConfig {
        /// DSI lane bitrate in megabits per second.
        pub lane_bit_rate_mbps: u32,
        /// DPI pixel clock in megahertz.
        pub dpi_clock_mhz: u32,
        /// Active and blanking timing.
        pub timing: DsiVideoTiming,
    }

    /// Narrow MIPI DSI/DPI binding implemented by a Platform HAL.
    ///
    /// Display implementations select the external controller, timing, and command
    /// sequence; the Platform owns the DSI PHY, framebuffer, and vendor HAL handles.
    pub trait DsiHost {
        /// Vendor HAL failure.
        type Error: core::fmt::Debug;

        /// Establishes the DSI link and configures the video data plane.
        fn initialize(&mut self, config: DsiPanelConfig) -> Result<(), Self::Error>;

        /// Sends one chip-specific command over the DSI DBI command channel.
        fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error>;

        /// Copies one row-major RGB565 rectangle into the DSI framebuffer.
        fn draw_rgb565(
            &mut self,
            x: u16,
            y: u16,
            width: u16,
            height: u16,
            pixels: &[u16],
        ) -> Result<(), Self::Error>;

        /// Enables or blanks panel output.
        fn set_display_enabled(&mut self, enabled: bool) -> Result<(), Self::Error>;

        /// Enters or leaves the controller's sleep state.
        fn set_sleep(&mut self, sleep: bool) -> Result<(), Self::Error>;

        /// Sets the PWM backlight on the portable `0..=255` scale.
        fn set_backlight(&mut self, brightness: u8) -> Result<(), Self::Error>;
    }

    /// Pixel storage format reported independently of a implementation's concrete
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
        /// The implementation can report or await controller readiness.
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

    /// Display power state requested by a peripheral consumer.
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
        /// Let the implementation select the appropriate policy.
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

    /// Barracuda's stable semantic interface for a attached display.
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

        /// Draws one row-major RGB888 region using implementation-owned color conversion.
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

    /// Move-only access to the Board's primary attached display.
    ///
    /// Generated Board HALs implement this trait when `board.yml` declares a
    /// attached peripheral whose peripheral is `display`.
    pub trait DisplayPeripheral {
        /// Concrete, statically dispatched display peripheral.
        type Display: Display;

        /// Transfers the primary display to its sole consumer once.
        fn take_display(&mut self) -> Option<Self::Display>;
    }
}

/// Stable indicator peripheral API.
pub mod indicator {
    /// Electrical level that represents the semantic on state.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ActiveLevel {
        /// The indicator is on while its output is low.
        Low,
        /// The indicator is on while its output is high.
        High,
    }

    /// Portable initialization configuration shared by indicator implementations.
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

    /// Move-only access to the Board's primary attached indicator.
    pub trait IndicatorPeripheral {
        /// Concrete, statically dispatched indicator peripheral.
        type Indicator: Indicator;

        /// Transfers the primary indicator to its sole consumer once.
        fn take_indicator(&mut self) -> Option<Self::Indicator>;
    }
}

/// Stable addressable LED-strip peripheral API.
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

    /// Move-only access to the Board's primary attached LED strip.
    pub trait LedStripPeripheral {
        /// Concrete, statically dispatched LED-strip peripheral.
        type LedStrip: LedStrip;

        /// Transfers the primary LED strip to its sole consumer once.
        fn take_led_strip(&mut self) -> Option<Self::LedStrip>;
    }
}

/// Stable camera peripheral API.
pub mod camera {
    use core::future::Future;

    /// Error family for a Platform-owned camera data receiver.
    pub trait FrameReceiverErrorType {
        /// Error returned while receiving one sensor frame.
        type Error: core::error::Error;
    }

    /// Narrow data-plane binding used by parallel camera sensor implementations.
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

    /// Pixel representation produced by a Camera implementation.
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

    /// Narrow MIPI-CSI/ISP host binding implemented by a Platform HAL.
    ///
    /// The camera implementation chooses the semantic stream format while the Platform
    /// owns the vendor video device, DMA buffers, and CSI receiver lifecycle.
    pub trait MipiCsiHost {
        /// Vendor video-stack failure.
        type Error: core::fmt::Debug;

        /// Initializes sensor discovery and the requested output stream.
        fn initialize(
            &mut self,
            descriptor: CameraDescriptor,
        ) -> impl Future<Output = Result<(), Self::Error>>;

        /// Captures one complete frame into caller-owned memory.
        fn capture<'a>(
            &'a mut self,
            buffer: &'a mut [u8],
        ) -> impl Future<Output = Result<usize, Self::Error>> + 'a;
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

    /// Semantic still-frame camera peripheral.
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

    /// Move-only access to the Board's primary attached camera.
    pub trait CameraPeripheral {
        /// Concrete, statically dispatched camera peripheral.
        type Camera: Camera;

        /// Transfers the primary camera to its sole consumer once.
        fn take_camera(&mut self) -> Option<Self::Camera>;
    }
}

/// Stable audio-codec peripheral API.
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

    /// Narrow PCM data-plane binding used by audio-codec implementations.
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

    /// Move-only access to the Board's primary attached audio codec.
    pub trait AudioCodecPeripheral {
        /// Concrete, statically dispatched codec peripheral.
        type AudioCodec: AudioCodec;

        /// Transfers the primary codec to its sole consumer once.
        fn take_audio_codec(&mut self) -> Option<Self::AudioCodec>;
    }
}

/// Stable inertial-measurement peripheral API.
pub mod imu {
    use core::future::Future;

    /// One signed three-axis measurement in a unit documented by its field.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Vector3 {
        /// Board-relative X component.
        pub x: i32,
        /// Board-relative Y component.
        pub y: i32,
        /// Board-relative Z component.
        pub z: i32,
    }

    impl Vector3 {
        /// Creates one three-axis vector.
        #[must_use]
        pub const fn new(x: i32, y: i32, z: i32) -> Self {
            Self { x, y, z }
        }
    }

    /// One source axis selected by an orientation transform.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SignedAxis {
        /// Positive source X.
        PositiveX,
        /// Negative source X.
        NegativeX,
        /// Positive source Y.
        PositiveY,
        /// Negative source Y.
        NegativeY,
        /// Positive source Z.
        PositiveZ,
        /// Negative source Z.
        NegativeZ,
    }

    impl SignedAxis {
        const fn select(self, value: Vector3) -> i32 {
            match self {
                Self::PositiveX => value.x,
                Self::NegativeX => value.x.saturating_neg(),
                Self::PositiveY => value.y,
                Self::NegativeY => value.y.saturating_neg(),
                Self::PositiveZ => value.z,
                Self::NegativeZ => value.z.saturating_neg(),
            }
        }
    }

    /// Signed axis selection from sensor-native axes into Board-relative axes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct AxisTransform {
        x: SignedAxis,
        y: SignedAxis,
        z: SignedAxis,
    }

    impl AxisTransform {
        /// Leaves all three sensor axes unchanged.
        pub const IDENTITY: Self = Self::new(
            SignedAxis::PositiveX,
            SignedAxis::PositiveY,
            SignedAxis::PositiveZ,
        );

        /// Creates an explicit signed axis selection.
        #[must_use]
        pub const fn new(x: SignedAxis, y: SignedAxis, z: SignedAxis) -> Self {
            Self { x, y, z }
        }

        /// Applies this mounting transform to one sensor-native vector.
        #[must_use]
        pub const fn apply(self, value: Vector3) -> Vector3 {
            Vector3::new(
                self.x.select(value),
                self.y.select(value),
                self.z.select(value),
            )
        }
    }

    impl Default for AxisTransform {
        fn default() -> Self {
            Self::IDENTITY
        }
    }

    /// Stable description of one configured six-axis IMU.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ImuDescriptor {
        accelerometer_rate_hz: u32,
        gyroscope_rate_hz: u32,
        accelerometer_range_mg: u32,
        gyroscope_range_mdps: u32,
        has_temperature: bool,
    }

    impl ImuDescriptor {
        /// Creates metadata for one initialized sensor profile.
        #[must_use]
        pub const fn new(
            accelerometer_rate_hz: u32,
            gyroscope_rate_hz: u32,
            accelerometer_range_mg: u32,
            gyroscope_range_mdps: u32,
            has_temperature: bool,
        ) -> Self {
            Self {
                accelerometer_rate_hz,
                gyroscope_rate_hz,
                accelerometer_range_mg,
                gyroscope_range_mdps,
                has_temperature,
            }
        }

        /// Returns the configured accelerometer sample rate in hertz.
        #[must_use]
        pub const fn accelerometer_rate_hz(self) -> u32 {
            self.accelerometer_rate_hz
        }

        /// Returns the configured gyroscope sample rate in hertz.
        #[must_use]
        pub const fn gyroscope_rate_hz(self) -> u32 {
            self.gyroscope_rate_hz
        }

        /// Returns the accelerometer full-scale magnitude in milli-g.
        #[must_use]
        pub const fn accelerometer_range_mg(self) -> u32 {
            self.accelerometer_range_mg
        }

        /// Returns the gyroscope full-scale magnitude in milli-degrees per second.
        #[must_use]
        pub const fn gyroscope_range_mdps(self) -> u32 {
            self.gyroscope_range_mdps
        }

        /// Returns whether samples include temperature.
        #[must_use]
        pub const fn has_temperature(self) -> bool {
            self.has_temperature
        }
    }

    /// One coherent inertial sample converted into portable integer units.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ImuSample {
        acceleration_mg: Vector3,
        angular_velocity_mdps: Vector3,
        temperature_mc: Option<i32>,
    }

    impl ImuSample {
        /// Creates one sample in milli-g, milli-degrees/second, and milli-Celsius.
        #[must_use]
        pub const fn new(
            acceleration_mg: Vector3,
            angular_velocity_mdps: Vector3,
            temperature_mc: Option<i32>,
        ) -> Self {
            Self {
                acceleration_mg,
                angular_velocity_mdps,
                temperature_mc,
            }
        }

        /// Returns Board-relative acceleration in milli-g.
        #[must_use]
        pub const fn acceleration_mg(self) -> Vector3 {
            self.acceleration_mg
        }

        /// Returns Board-relative angular velocity in milli-degrees per second.
        #[must_use]
        pub const fn angular_velocity_mdps(self) -> Vector3 {
            self.angular_velocity_mdps
        }

        /// Returns die temperature in milli-degrees Celsius when available.
        #[must_use]
        pub const fn temperature_mc(self) -> Option<i32> {
            self.temperature_mc
        }
    }

    /// Semantic six-axis inertial sensor peripheral.
    pub trait Imu {
        /// Concrete register or sampling failure.
        type Error: core::fmt::Debug;

        /// Returns the configured sensor profile.
        fn descriptor(&self) -> ImuDescriptor;

        /// Reads one coherent accelerometer, gyroscope, and temperature sample.
        fn read_sample(&mut self) -> impl Future<Output = Result<ImuSample, Self::Error>> + '_;
    }

    /// Move-only access to the Board's primary attached IMU.
    pub trait ImuPeripheral {
        /// Concrete, statically dispatched inertial peripheral.
        type Imu: Imu;

        /// Transfers the primary IMU to its sole consumer once.
        fn take_imu(&mut self) -> Option<Self::Imu>;
    }
}
