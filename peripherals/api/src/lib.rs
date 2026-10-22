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

/// One claimed device of a peripheral kind on the selected Board.
///
/// A Plugin's build resolves its `plugin.toml` peripheral slots against the
/// Board and receives each claimed device as an `Instance` constant: the
/// device's position among the Board's devices of that kind, in name order.
/// Taking an instance moves that device out once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Instance(u8);

impl Instance {
    /// Names the device at `index` among the Board's devices of one kind.
    #[must_use]
    pub const fn new(index: u8) -> Self {
        Self(index)
    }

    /// Returns the device's position among the Board's devices of its kind.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

mod sets;

#[doc(hidden)]
pub mod __private {
    pub use embedded_graphics_core;
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

    /// A battery's charge as its fuel gauge reports it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct BatteryState {
        /// State of charge in percent, 0 through 100.
        pub percent: u8,
        /// Whether current flows into the battery.
        pub charging: bool,
    }

    /// Semantic voltage, current, and power monitor peripheral.
    pub trait PowerMonitor {
        /// Concrete register or sampling failure.
        type Error: core::fmt::Debug;

        /// Reads one coherent electrical measurement.
        fn measure(&mut self) -> Result<PowerMeasurement, Self::Error>;

        /// Reads the battery's charge; `None` from a monitor that is no
        /// battery fuel gauge.
        fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
            Ok(None)
        }
    }

    /// Move-only access to the Board's attached power monitor.
    pub trait PowerMonitorPeripheral {
        /// Concrete, statically dispatched power-monitor peripheral.
        type PowerMonitor: PowerMonitor;

        /// Transfers the claimed power monitor `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_power_monitor(&mut self, instance: crate::Instance) -> Option<Self::PowerMonitor>;
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

        /// Transfers the claimed real-time clock `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_real_time_clock(
            &mut self,
            instance: crate::Instance,
        ) -> Option<Self::RealTimeClock>;
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

        /// Transfers the claimed touch input `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_touch(&mut self, instance: crate::Instance) -> Option<Self::Touch>;
    }
}

/// Stable attached-button peripheral API.
pub mod buttons {
    /// Maximum buttons represented by one portable snapshot.
    pub const MAX_BUTTONS: usize = 8;

    /// Simultaneously sampled attached-button state.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ButtonSnapshot {
        pressed: [bool; MAX_BUTTONS],
        count: u8,
    }

    impl ButtonSnapshot {
        /// Creates a snapshot from a dense prefix of button states.
        #[must_use]
        pub fn new(states: &[bool]) -> Self {
            let mut snapshot = Self::default();
            for (target, source) in snapshot.pressed.iter_mut().zip(states) {
                *target = *source;
                snapshot.count = snapshot.count.saturating_add(1);
            }
            snapshot
        }

        /// Returns the number of attached buttons.
        #[must_use]
        pub const fn len(self) -> usize {
            self.count as usize
        }

        /// Returns whether no buttons are attached.
        #[must_use]
        pub const fn is_empty(self) -> bool {
            self.count == 0
        }

        /// Returns one button state by stable zero-based index.
        #[must_use]
        pub const fn is_pressed(self, index: usize) -> Option<bool> {
            if index < self.count as usize {
                Some(self.pressed[index])
            } else {
                None
            }
        }
    }

    /// Semantic attached-button input peripheral.
    pub trait Buttons {
        /// Concrete sampling failure.
        type Error: core::fmt::Debug;

        /// Reads all button states from one hardware sample.
        fn read(&mut self) -> Result<ButtonSnapshot, Self::Error>;
    }

    /// Move-only access to the Board's attached buttons.
    pub trait ButtonsPeripheral {
        /// Concrete, statically dispatched button implementation.
        type Buttons: Buttons;

        /// Transfers the claimed button peripheral `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_buttons(&mut self, instance: crate::Instance) -> Option<Self::Buttons>;
    }
}

/// Stable display peripheral API.
pub mod display {
    use core::{future::Future, ops::BitOr};

    use embedded_graphics_core::{
        draw_target::DrawTarget,
        geometry::{OriginDimensions, Size},
        pixelcolor::Rgb565,
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
    ///
    /// The link starts in command mode: [`DsiHost::initialize`] brings it up,
    /// and [`DsiHost::start_video`] starts streaming the framebuffer. Commands
    /// may be written in either mode, so a panel whose controller expects its
    /// initialization sequence before video, or during it, is served by
    /// ordering the two calls.
    pub trait DsiHost {
        /// Vendor HAL failure.
        type Error: core::fmt::Debug;

        /// Establishes the DSI link in command mode and records the video timing.
        fn initialize(&mut self, config: DsiPanelConfig) -> Result<(), Self::Error>;

        /// Starts streaming the framebuffer to the panel in video mode.
        ///
        /// Starting a stream that already runs does nothing.
        fn start_video(&mut self) -> Result<(), Self::Error>;

        /// Sends one chip-specific command over the DSI DBI command channel.
        fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error>;

        /// Makes every pixel drawn since the last call visible to the stream.
        fn present(&mut self) -> Result<(), Self::Error>;

        /// Copies one row-major RGB565 rectangle into the DSI framebuffer.
        ///
        /// The pixels reach the panel at the next [`DsiHost::present`].
        fn draw_rgb565(
            &mut self,
            x: u16,
            y: u16,
            width: u16,
            height: u16,
            pixels: &[u16],
        ) -> Result<(), Self::Error>;

        /// Enables or blanks panel output with the DCS display on/off commands.
        fn set_display_enabled(&mut self, enabled: bool) -> Result<(), Self::Error>;

        /// Enters or leaves the controller's sleep state.
        fn set_sleep(&mut self, sleep: bool) -> Result<(), Self::Error>;

        /// Sets the backlight on the portable `0..=255` scale.
        ///
        /// A Board that switches its backlight pin without PWM treats every
        /// non-zero level as on. A host whose Board drives the backlight
        /// through another chip reports an error; the display implementation
        /// that owns that chip controls it instead.
        fn set_backlight(&mut self, brightness: u8) -> Result<(), Self::Error>;
    }

    /// Narrow eight-bit Intel 8080-style parallel output binding implemented
    /// by a Platform HAL.
    ///
    /// The Platform owns the controller, its DMA, and the pin routing: a write
    /// strobe, an active-low chip select asserted for each burst, eight data
    /// lines, and the data/command line when the Board wires one. A MIPI DBI
    /// panel controller takes commands with data/command low and their
    /// parameters and pixels with it high. A directly driven e-paper source
    /// driver takes raw line bytes, its source clock on the write strobe and
    /// its source start pulse on the chip select; its display implementation
    /// drives every other timing line and the waveform itself.
    pub trait ParallelOutput {
        /// Vendor HAL failure.
        type Error: core::fmt::Debug;

        /// Largest burst in bytes clocked out under one chip-select
        /// assertion.
        fn capacity(&self) -> usize;

        /// Clocks one command byte with data/command low, then its payload
        /// with data/command high.
        ///
        /// A bus without a data/command line reports an error.
        fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error>;

        /// Clocks `data` with data/command high, data line 0 carrying bit 0 of
        /// each byte, and returns once the last byte has left the bus.
        ///
        /// Data no longer than [`ParallelOutput::capacity`] goes out as one
        /// burst; longer data as consecutive bursts of at most that many
        /// bytes, the chip select released between them. A consumer that
        /// needs one unbroken burst keeps each write within the capacity.
        fn write_data(&mut self, data: &[u8]) -> Result<(), Self::Error>;
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
        /// Only the circle inscribed in the frame is visible.
        pub const ROUND: Self = Self(1 << 7);

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

        /// Draws one row-major RGB565 region, converted by the implementation
        /// when its native pixel format differs.
        fn draw_rgb565(
            &mut self,
            area: Rectangle,
            pixels: &[Rgb565],
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

    /// Move-only access to the Board's attached display.
    ///
    /// Generated Board HALs implement this trait when `board.yml` declares a
    /// attached peripheral whose peripheral is `display`.
    pub trait DisplayPeripheral {
        /// Concrete, statically dispatched display peripheral.
        type Display: Display;

        /// Transfers the claimed display `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_display(&mut self, instance: crate::Instance) -> Option<Self::Display>;
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

    /// Move-only access to the Board's attached indicator.
    pub trait IndicatorPeripheral {
        /// Concrete, statically dispatched indicator peripheral.
        type Indicator: Indicator;

        /// Transfers the claimed indicator `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_indicator(&mut self, instance: crate::Instance) -> Option<Self::Indicator>;
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

    /// Move-only access to the Board's attached LED strip.
    pub trait LedStripPeripheral {
        /// Concrete, statically dispatched LED-strip peripheral.
        type LedStrip: LedStrip;

        /// Transfers the claimed LED strip `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_led_strip(&mut self, instance: crate::Instance) -> Option<Self::LedStrip>;
    }
}

/// Stable buzzer peripheral API.
pub mod buzzer {
    use core::future::Future;

    /// Loudness of a tone, in percent of the loudest the buzzer makes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Volume(u8);

    impl Volume {
        /// The loudest tone.
        pub const MAX: Self = Self(100);

        /// Creates a volume from a percentage; values above 100 mean 100.
        #[must_use]
        pub const fn percent(percent: u8) -> Self {
            if percent > 100 {
                Self::MAX
            } else {
                Self(percent)
            }
        }

        /// Returns the volume in percent, 0 through 100.
        #[must_use]
        pub const fn get(self) -> u8 {
            self.0
        }
    }

    /// One note of a melody: a tone, or a rest, held for a duration.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Note {
        frequency_hz: u32,
        duration_ms: u32,
    }

    impl Note {
        /// Creates a tone of `frequency_hz` held for `duration_ms`; a zero
        /// frequency is a rest.
        #[must_use]
        pub const fn new(frequency_hz: u32, duration_ms: u32) -> Self {
            Self {
                frequency_hz,
                duration_ms,
            }
        }

        /// Creates a silent rest of `duration_ms`.
        #[must_use]
        pub const fn rest(duration_ms: u32) -> Self {
            Self::new(0, duration_ms)
        }

        /// Returns the tone frequency in hertz; zero for a rest.
        #[must_use]
        pub const fn frequency_hz(self) -> u32 {
            self.frequency_hz
        }

        /// Returns how long the note lasts in milliseconds.
        #[must_use]
        pub const fn duration_ms(self) -> u32 {
            self.duration_ms
        }

        /// Returns whether the note is a rest.
        #[must_use]
        pub const fn is_rest(self) -> bool {
            self.frequency_hz == 0
        }
    }

    /// Semantic tone output of a passive buzzer.
    ///
    /// A tone keeps sounding until [`Buzzer::stop`] or the next tone. A note
    /// future silences the buzzer when it completes and when it is dropped,
    /// so cancelling a melody never leaves a tone sounding.
    pub trait Buzzer {
        /// Concrete output failure.
        type Error: core::fmt::Debug;

        /// Returns the lowest and highest tone frequency in hertz.
        fn frequency_range(&self) -> (u32, u32);

        /// Sounds `frequency_hz` at `volume` until stopped.
        fn start(&mut self, frequency_hz: u32, volume: Volume) -> Result<(), Self::Error>;

        /// Silences the buzzer.
        fn stop(&mut self) -> Result<(), Self::Error>;

        /// Sounds one note for its duration, then silences the buzzer.
        fn play_note(
            &mut self,
            note: Note,
            volume: Volume,
        ) -> impl Future<Output = Result<(), Self::Error>>;

        /// Plays `melody` note by note at `volume`.
        fn play(
            &mut self,
            melody: &[Note],
            volume: Volume,
        ) -> impl Future<Output = Result<(), Self::Error>> {
            async move {
                for note in melody {
                    self.play_note(*note, volume).await?;
                }
                Ok(())
            }
        }
    }

    /// Move-only access to the Board's attached buzzer.
    pub trait BuzzerPeripheral {
        /// Concrete, statically dispatched buzzer peripheral.
        type Buzzer: Buzzer;

        /// Transfers the claimed buzzer `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_buzzer(&mut self, instance: crate::Instance) -> Option<Self::Buzzer>;
    }
}

/// Stable vibration-motor peripheral API.
pub mod vibration {
    use core::future::Future;

    /// Motor drive strength, in percent of the strongest.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Strength(u8);

    impl Strength {
        /// The motor stopped.
        pub const OFF: Self = Self(0);
        /// The strongest vibration.
        pub const MAX: Self = Self(100);

        /// Creates a strength from a percentage; values above 100 mean 100.
        #[must_use]
        pub const fn percent(percent: u8) -> Self {
            if percent > 100 {
                Self::MAX
            } else {
                Self(percent)
            }
        }

        /// Returns the strength in percent, 0 through 100.
        #[must_use]
        pub const fn get(self) -> u8 {
            self.0
        }

        /// Returns whether the motor is stopped at this strength.
        #[must_use]
        pub const fn is_off(self) -> bool {
            self.0 == 0
        }
    }

    /// One step of a vibration pattern: the motor runs, then rests.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Pulse {
        on_ms: u32,
        off_ms: u32,
    }

    impl Pulse {
        /// Creates a step that runs the motor for `on_ms`, then rests for
        /// `off_ms`.
        #[must_use]
        pub const fn new(on_ms: u32, off_ms: u32) -> Self {
            Self { on_ms, off_ms }
        }

        /// Returns how long the motor runs, in milliseconds.
        #[must_use]
        pub const fn on_ms(self) -> u32 {
            self.on_ms
        }

        /// Returns how long the motor rests afterwards, in milliseconds.
        #[must_use]
        pub const fn off_ms(self) -> u32 {
            self.off_ms
        }
    }

    /// Semantic vibration motor.
    ///
    /// A motor without strength control runs at full strength for any
    /// non-zero [`Strength`]. A pattern future stops the motor when it
    /// completes and when it is dropped.
    pub trait Vibration {
        /// Concrete output failure.
        type Error: core::fmt::Debug;

        /// Returns whether the motor's strength is adjustable.
        fn has_variable_strength(&self) -> bool;

        /// Runs the motor at `strength` until changed; [`Strength::OFF`]
        /// stops it.
        fn set(&mut self, strength: Strength) -> Result<(), Self::Error>;

        /// Runs `pattern` once at `strength`, then stops the motor.
        fn pulse(
            &mut self,
            pattern: &[Pulse],
            strength: Strength,
        ) -> impl Future<Output = Result<(), Self::Error>>;
    }

    /// Move-only access to the Board's attached vibration motor.
    pub trait VibrationPeripheral {
        /// Concrete, statically dispatched vibration peripheral.
        type Vibration: Vibration;

        /// Transfers the claimed vibration motor `instance` to its Plugin
        /// once; `None` when the Board has no such device or it was already
        /// taken.
        fn take_vibration(&mut self, instance: crate::Instance) -> Option<Self::Vibration>;
    }
}

/// Stable infrared remote-control peripheral API.
pub mod infrared {
    use core::future::Future;

    /// One NEC remote-control command.
    ///
    /// Standard NEC sends an eight-bit address and its inverse; extended NEC
    /// uses all sixteen address bits. The command is always followed by its
    /// inverse on the wire.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct NecCommand {
        address: u16,
        command: u8,
        extended: bool,
    }

    impl NecCommand {
        /// Creates a standard NEC command with an eight-bit address.
        #[must_use]
        pub const fn new(address: u8, command: u8) -> Self {
            Self {
                address: address as u16,
                command,
                extended: false,
            }
        }

        /// Creates an extended NEC command with a sixteen-bit address, sent
        /// low byte first.
        #[must_use]
        pub const fn extended(address: u16, command: u8) -> Self {
            Self {
                address,
                command,
                extended: true,
            }
        }

        /// Returns the address: eight bits, or sixteen when extended.
        #[must_use]
        pub const fn address(self) -> u16 {
            self.address
        }

        /// Returns the command byte.
        #[must_use]
        pub const fn command(self) -> u8 {
            self.command
        }

        /// Returns whether the address uses all sixteen bits.
        #[must_use]
        pub const fn is_extended(self) -> bool {
            self.extended
        }
    }

    /// Carrier a transmitter modulates its marks with.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Carrier {
        frequency_hz: u32,
        duty_percent: u8,
    }

    impl Carrier {
        /// The 38 kHz, one-third duty carrier of NEC and most consumer remotes.
        pub const NEC: Self = Self::new(38_000, 33);

        /// Creates a carrier of `frequency_hz` lit for `duty_percent` of each
        /// period; duties above 100 mean 100.
        #[must_use]
        pub const fn new(frequency_hz: u32, duty_percent: u8) -> Self {
            Self {
                frequency_hz,
                duty_percent: if duty_percent > 100 {
                    100
                } else {
                    duty_percent
                },
            }
        }

        /// Returns the carrier frequency in hertz.
        #[must_use]
        pub const fn frequency_hz(self) -> u32 {
            self.frequency_hz
        }

        /// Returns the lit share of each period in percent.
        #[must_use]
        pub const fn duty_percent(self) -> u8 {
            self.duty_percent
        }
    }

    /// One frame a receiver captured.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Received {
        /// A complete NEC command.
        Nec(NecCommand),
        /// The NEC repeat code a held button sends.
        NecRepeat,
        /// A frame of another protocol: its first `len` timings, alternating
        /// mark and space in microseconds and starting with a mark, are in
        /// the caller's buffer.
        Raw {
            /// Number of timings written.
            len: usize,
        },
    }

    /// Semantic infrared transmitter, with a receiver where the Board has one.
    pub trait Infrared {
        /// Concrete transfer failure, including a receive on a Board without
        /// a receiver.
        type Error: core::fmt::Debug;

        /// Returns whether the Board has an infrared receiver.
        fn can_receive(&self) -> bool;

        /// Sends one NEC command on the 38 kHz NEC carrier.
        fn send_nec(
            &mut self,
            command: NecCommand,
        ) -> impl Future<Output = Result<(), Self::Error>>;

        /// Sends a pulse train: `timings_us` alternates mark and space in
        /// microseconds, starting with a mark, and marks carry `carrier`.
        fn send_raw(
            &mut self,
            carrier: Carrier,
            timings_us: &[u16],
        ) -> impl Future<Output = Result<(), Self::Error>>;

        /// Waits for the next frame. An NEC frame is decoded; any other frame
        /// leaves its timings in `timings_us`, mark first, truncated to its
        /// length.
        fn receive(
            &mut self,
            timings_us: &mut [u16],
        ) -> impl Future<Output = Result<Received, Self::Error>>;
    }

    /// Move-only access to the Board's attached infrared transceiver.
    pub trait InfraredPeripheral {
        /// Concrete, statically dispatched infrared peripheral.
        type Infrared: Infrared;

        /// Transfers the claimed infrared device `instance` to its Plugin
        /// once; `None` when the Board has no such device or it was already
        /// taken.
        fn take_infrared(&mut self, instance: crate::Instance) -> Option<Self::Infrared>;
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

    /// Move-only access to the Board's attached camera.
    pub trait CameraPeripheral {
        /// Concrete, statically dispatched camera peripheral.
        type Camera: Camera;

        /// Transfers the claimed camera `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_camera(&mut self, instance: crate::Instance) -> Option<Self::Camera>;
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

    /// Move-only access to the Board's attached audio codec.
    pub trait AudioCodecPeripheral {
        /// Concrete, statically dispatched codec peripheral.
        type AudioCodec: AudioCodec;

        /// Transfers the claimed audio codec `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_audio_codec(&mut self, instance: crate::Instance) -> Option<Self::AudioCodec>;
    }
}

/// Stable keyboard peripheral API.
pub mod keyboard {
    use core::ops::BitOr;

    /// One key, named by what it does rather than where it sits.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum Key {
        /// A key whose legend is a printable character, named by its
        /// unshifted legend: `'a'`, `'1'`, `';'`, or `' '` for the space bar.
        Character(char),
        /// Enter or Return.
        Enter,
        /// Backspace, deleting before the cursor.
        Backspace,
        /// Delete, deleting after the cursor.
        Delete,
        /// Tab.
        Tab,
        /// Escape.
        Escape,
        /// Shift, or a key that shifts the next letters, such as `Aa`.
        Shift,
        /// Control.
        Control,
        /// Alt.
        Alt,
        /// Option, or a vendor's equivalent second modifier.
        Option,
        /// Function, the modifier that selects a keyboard's second layer.
        Function,
        /// Cursor up.
        Up,
        /// Cursor down.
        Down,
        /// Cursor left.
        Left,
        /// Cursor right.
        Right,
        /// Function key `F1` through `F24`, by number.
        F(u8),
    }

    /// Modifier keys held when an event happened.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Modifiers(u8);

    impl Modifiers {
        /// Shift.
        pub const SHIFT: Self = Self(1 << 0);
        /// Control.
        pub const CONTROL: Self = Self(1 << 1);
        /// Alt.
        pub const ALT: Self = Self(1 << 2);
        /// Option.
        pub const OPTION: Self = Self(1 << 3);
        /// Function.
        pub const FUNCTION: Self = Self(1 << 4);

        /// Returns no modifiers.
        #[must_use]
        pub const fn empty() -> Self {
            Self(0)
        }

        /// Returns whether every modifier in `other` is held.
        #[must_use]
        pub const fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }

        /// Returns the modifiers held in either set.
        #[must_use]
        pub const fn union(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }

        /// Returns these modifiers without those in `other`.
        #[must_use]
        pub const fn difference(self, other: Self) -> Self {
            Self(self.0 & !other.0)
        }

        /// Returns the modifier a modifier key holds, or `None` for any
        /// other key.
        #[must_use]
        pub const fn of(key: Key) -> Option<Self> {
            match key {
                Key::Shift => Some(Self::SHIFT),
                Key::Control => Some(Self::CONTROL),
                Key::Alt => Some(Self::ALT),
                Key::Option => Some(Self::OPTION),
                Key::Function => Some(Self::FUNCTION),
                _ => None,
            }
        }
    }

    impl BitOr for Modifiers {
        type Output = Self;

        fn bitor(self, rhs: Self) -> Self::Output {
            self.union(rhs)
        }
    }

    /// Whether a key went down or came up.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum KeyState {
        /// The key went down.
        Pressed,
        /// The key came up.
        Released,
    }

    /// One key going down or coming up, translated by the keyboard's keymap.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KeyEvent {
        key: Key,
        state: KeyState,
        modifiers: Modifiers,
        text: Option<char>,
        code: u16,
    }

    impl KeyEvent {
        /// Creates one event.
        #[must_use]
        pub const fn new(
            key: Key,
            state: KeyState,
            modifiers: Modifiers,
            text: Option<char>,
            code: u16,
        ) -> Self {
            Self {
                key,
                state,
                modifiers,
                text,
                code,
            }
        }

        /// Returns what the key does on the layer the held modifiers select.
        #[must_use]
        pub const fn key(self) -> Key {
            self.key
        }

        /// Returns whether the key went down or came up.
        #[must_use]
        pub const fn state(self) -> KeyState {
            self.state
        }

        /// Returns whether the key went down.
        #[must_use]
        pub const fn is_pressed(self) -> bool {
            matches!(self.state, KeyState::Pressed)
        }

        /// Returns the modifiers held after this event.
        #[must_use]
        pub const fn modifiers(self) -> Modifiers {
            self.modifiers
        }

        /// Returns the character a press types under the keyboard's keymap
        /// and the held modifiers; `None` for a release and for keys that
        /// type nothing, such as modifiers, arrows, Enter, and Backspace.
        #[must_use]
        pub const fn text(self) -> Option<char> {
            self.text
        }

        /// Returns the key's position on this keyboard, stable for one
        /// Board and meaningless across Boards.
        #[must_use]
        pub const fn code(self) -> u16 {
            self.code
        }
    }

    /// Semantic keyboard peripheral.
    ///
    /// The implementation owns the key matrix or controller, its keymap, and
    /// the modifier state. A keyboard scanned by the SoC samples its matrix
    /// when polled; one behind a controller with an event queue reads that
    /// queue, using the controller's interrupt line, when the Board wires it,
    /// to skip the bus while nothing happened.
    pub trait Keyboard {
        /// Concrete scanning or bus failure.
        type Error: core::fmt::Debug;

        /// Returns the oldest key event not yet returned, without waiting;
        /// `None` when no key changed.
        fn poll_event(&mut self) -> Result<Option<KeyEvent>, Self::Error>;
    }

    /// Move-only access to the Board's attached keyboard.
    pub trait KeyboardPeripheral {
        /// Concrete, statically dispatched keyboard implementation.
        type Keyboard: Keyboard;

        /// Transfers the claimed keyboard `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_keyboard(&mut self, instance: crate::Instance) -> Option<Self::Keyboard>;
    }
}

/// Stable rotary-encoder peripheral API.
pub mod rotary_encoder {
    /// Semantic incremental rotary encoder.
    ///
    /// The position counts the encoder's quadrature edges, signed by the
    /// direction of turn; the Board page says how many make one detent when
    /// the vendor documents it. A push button on the knob is a button of the
    /// Board's `buttons` peripheral, not part of this one.
    pub trait RotaryEncoder {
        /// Concrete counter failure.
        type Error: core::fmt::Debug;

        /// Returns the signed position in edges since start or the last
        /// [`RotaryEncoder::reset`].
        fn position(&mut self) -> Result<i64, Self::Error>;

        /// Returns the edges turned since the previous call, or since start
        /// or the last [`RotaryEncoder::reset`] for the first.
        fn delta(&mut self) -> Result<i64, Self::Error>;

        /// Makes the current position zero.
        fn reset(&mut self) -> Result<(), Self::Error>;
    }

    /// Move-only access to the Board's attached rotary encoder.
    pub trait RotaryEncoderPeripheral {
        /// Concrete, statically dispatched rotary-encoder implementation.
        type RotaryEncoder: RotaryEncoder;

        /// Transfers the claimed rotary encoder `instance` to its Plugin once;
        /// `None` when the Board has no such device or it was already taken.
        fn take_rotary_encoder(&mut self, instance: crate::Instance)
        -> Option<Self::RotaryEncoder>;
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
        const fn source_mask(self) -> u8 {
            match self {
                Self::PositiveX | Self::NegativeX => 0b001,
                Self::PositiveY | Self::NegativeY => 0b010,
                Self::PositiveZ | Self::NegativeZ => 0b100,
            }
        }

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

        /// Returns whether every sensor axis is selected exactly once, so the
        /// transform is a rotation or a reflection of the sensor's axes.
        #[must_use]
        pub const fn is_permutation(self) -> bool {
            let mask = self.x.source_mask() | self.y.source_mask() | self.z.source_mask();
            mask == 0b111
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

    /// Stable description of one configured six-axis IMU, and of the
    /// magnetometer read with it when the Board has one.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ImuDescriptor {
        accelerometer_rate_hz: u32,
        gyroscope_rate_hz: u32,
        accelerometer_range_mg: u32,
        gyroscope_range_mdps: u32,
        has_temperature: bool,
        magnetometer_rate_hz: Option<u32>,
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
                magnetometer_rate_hz: None,
            }
        }

        /// Adds a magnetometer that measures `rate_hz` times a second; its
        /// field reaches each sample through [`ImuSample::magnetic_field_nt`].
        #[must_use]
        pub const fn with_magnetometer(self, rate_hz: u32) -> Self {
            Self {
                magnetometer_rate_hz: Some(rate_hz),
                ..self
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

        /// Returns the magnetometer's measurement rate in hertz, or `None`
        /// when the IMU has no magnetometer.
        #[must_use]
        pub const fn magnetometer_rate_hz(self) -> Option<u32> {
            self.magnetometer_rate_hz
        }

        /// Returns whether samples include the magnetic field.
        #[must_use]
        pub const fn has_magnetometer(self) -> bool {
            self.magnetometer_rate_hz.is_some()
        }
    }

    /// One coherent inertial sample converted into portable integer units.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ImuSample {
        acceleration_mg: Vector3,
        angular_velocity_mdps: Vector3,
        temperature_mc: Option<i32>,
        magnetic_field_nt: Option<Vector3>,
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
                magnetic_field_nt: None,
            }
        }

        /// Adds the Board-relative magnetic field in nanotesla, in the same
        /// axes as acceleration and angular velocity.
        #[must_use]
        pub const fn with_magnetic_field(self, magnetic_field_nt: Vector3) -> Self {
            Self {
                magnetic_field_nt: Some(magnetic_field_nt),
                ..self
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

        /// Returns the Board-relative magnetic field in nanotesla: `None`
        /// when the IMU has no magnetometer, or when the field saturated the
        /// magnetometer for this sample.
        #[must_use]
        pub const fn magnetic_field_nt(self) -> Option<Vector3> {
            self.magnetic_field_nt
        }
    }

    /// Semantic six-axis inertial sensor peripheral, with the three-axis
    /// magnetometer its implementation reads beside it when the Board has one.
    ///
    /// The magnetometer is a channel of the IMU rather than a peripheral of
    /// its own: M5Stack fits the BMM150 behind the BMI270's auxiliary bus,
    /// where only the IMU's implementation can reach it, and its field is
    /// useful only in the IMU's axes.
    pub trait Imu {
        /// Concrete register or sampling failure.
        type Error: core::fmt::Debug;

        /// Returns the configured sensor profile.
        fn descriptor(&self) -> ImuDescriptor;

        /// Reads one coherent accelerometer, gyroscope, temperature, and,
        /// when present, magnetic-field sample.
        fn read_sample(&mut self) -> impl Future<Output = Result<ImuSample, Self::Error>> + '_;
    }

    /// Move-only access to the Board's attached IMU.
    pub trait ImuPeripheral {
        /// Concrete, statically dispatched inertial peripheral.
        type Imu: Imu;

        /// Transfers the claimed IMU `instance` to its Plugin once; `None`
        /// when the Board has no such device or it was already taken.
        fn take_imu(&mut self, instance: crate::Instance) -> Option<Self::Imu>;
    }
}

/// Stable environmental-sensing peripheral API.
///
/// One environment sensor reports whichever ambient quantities its chips
/// measure: air temperature and humidity, CO2, particulate matter, VOC and
/// NOx indices, illuminance, and proximity. A product with several such
/// chips presents them as one device whose implementation composes the
/// chips, so its [`Quantities`] are the union of theirs.
pub mod environment {
    use core::future::Future;

    /// One ambient quantity an environment sensor can report, with the
    /// integer unit [`EnvironmentReading`] carries it in.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub enum Quantity {
        /// Air temperature in milli-degrees Celsius.
        Temperature,
        /// Relative humidity in thousandths of a percent.
        RelativeHumidity,
        /// Carbon dioxide concentration in parts per million.
        Co2,
        /// PM1.0 particulate mass concentration in nanograms per cubic metre.
        Pm1_0,
        /// PM2.5 particulate mass concentration in nanograms per cubic metre.
        Pm2_5,
        /// PM4.0 particulate mass concentration in nanograms per cubic metre.
        Pm4_0,
        /// PM10 particulate mass concentration in nanograms per cubic metre.
        Pm10,
        /// Sensirion VOC index (1 through 500) in thousandths.
        VocIndex,
        /// Sensirion NOx index (1 through 500) in thousandths.
        NoxIndex,
        /// Ambient illuminance in millilux.
        Illuminance,
        /// Proximity in the sensor's raw counts; larger is nearer.
        Proximity,
    }

    impl Quantity {
        /// Every quantity, in the stable order [`Quantities::iter`] uses.
        pub const ALL: [Self; 11] = [
            Self::Temperature,
            Self::RelativeHumidity,
            Self::Co2,
            Self::Pm1_0,
            Self::Pm2_5,
            Self::Pm4_0,
            Self::Pm10,
            Self::VocIndex,
            Self::NoxIndex,
            Self::Illuminance,
            Self::Proximity,
        ];

        /// Returns the stable lowercase name scripts and logs use.
        #[must_use]
        pub const fn name(self) -> &'static str {
            match self {
                Self::Temperature => "temperature",
                Self::RelativeHumidity => "relative_humidity",
                Self::Co2 => "co2",
                Self::Pm1_0 => "pm1_0",
                Self::Pm2_5 => "pm2_5",
                Self::Pm4_0 => "pm4_0",
                Self::Pm10 => "pm10",
                Self::VocIndex => "voc_index",
                Self::NoxIndex => "nox_index",
                Self::Illuminance => "illuminance",
                Self::Proximity => "proximity",
            }
        }

        /// Finds the quantity with the stable `name`.
        #[must_use]
        pub fn from_name(name: &str) -> Option<Self> {
            Self::ALL
                .into_iter()
                .find(|quantity| quantity.name() == name)
        }

        const fn bit(self) -> u16 {
            1 << (self as u16)
        }
    }

    /// A set of quantities: what a sensor reports.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Quantities(u16);

    impl Quantities {
        /// The empty set.
        pub const EMPTY: Self = Self(0);

        /// Returns this set with `quantity` added.
        #[must_use]
        pub const fn with(self, quantity: Quantity) -> Self {
            Self(self.0 | quantity.bit())
        }

        /// Returns every quantity in either set.
        #[must_use]
        pub const fn union(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }

        /// Returns whether the set holds `quantity`.
        #[must_use]
        pub const fn contains(self, quantity: Quantity) -> bool {
            self.0 & quantity.bit() != 0
        }

        /// Returns whether the set holds no quantity.
        #[must_use]
        pub const fn is_empty(self) -> bool {
            self.0 == 0
        }

        /// Returns the number of quantities in the set.
        #[must_use]
        pub const fn len(self) -> usize {
            self.0.count_ones() as usize
        }

        /// Iterates the set in [`Quantity::ALL`] order.
        pub fn iter(self) -> impl Iterator<Item = Quantity> {
            Quantity::ALL
                .into_iter()
                .filter(move |quantity| self.contains(*quantity))
        }
    }

    impl FromIterator<Quantity> for Quantities {
        fn from_iter<I: IntoIterator<Item = Quantity>>(iter: I) -> Self {
            iter.into_iter().fold(Self::EMPTY, Self::with)
        }
    }

    /// One reading in integer SI subunits. A quantity the sensor does not
    /// measure, or has no valid value for yet (a CO2 sensor's first
    /// measurement period, a gas index's warm-up), is `None`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct EnvironmentReading {
        /// Air temperature in milli-degrees Celsius.
        pub temperature_millicelsius: Option<i32>,
        /// Relative humidity in thousandths of a percent, 0 through 100 000.
        pub relative_humidity_millipercent: Option<u32>,
        /// Carbon dioxide concentration in parts per million.
        pub co2_ppm: Option<u32>,
        /// PM1.0 mass concentration in nanograms per cubic metre.
        pub pm1_0_ng_per_m3: Option<u32>,
        /// PM2.5 mass concentration in nanograms per cubic metre.
        pub pm2_5_ng_per_m3: Option<u32>,
        /// PM4.0 mass concentration in nanograms per cubic metre.
        pub pm4_0_ng_per_m3: Option<u32>,
        /// PM10 mass concentration in nanograms per cubic metre.
        pub pm10_ng_per_m3: Option<u32>,
        /// VOC index in thousandths, 1 000 through 500 000.
        pub voc_index_milli: Option<u32>,
        /// NOx index in thousandths, 1 000 through 500 000.
        pub nox_index_milli: Option<u32>,
        /// Ambient illuminance in millilux.
        pub illuminance_millilux: Option<u32>,
        /// Proximity in the sensor's raw counts; larger is nearer.
        pub proximity_counts: Option<u32>,
    }

    impl EnvironmentReading {
        /// A reading with no value.
        pub const EMPTY: Self = Self {
            temperature_millicelsius: None,
            relative_humidity_millipercent: None,
            co2_ppm: None,
            pm1_0_ng_per_m3: None,
            pm2_5_ng_per_m3: None,
            pm4_0_ng_per_m3: None,
            pm10_ng_per_m3: None,
            voc_index_milli: None,
            nox_index_milli: None,
            illuminance_millilux: None,
            proximity_counts: None,
        };

        /// Returns one quantity's value in its [`Quantity`] unit.
        #[must_use]
        pub fn value(&self, quantity: Quantity) -> Option<i64> {
            let unsigned = match quantity {
                Quantity::Temperature => {
                    return self.temperature_millicelsius.map(i64::from);
                }
                Quantity::RelativeHumidity => self.relative_humidity_millipercent,
                Quantity::Co2 => self.co2_ppm,
                Quantity::Pm1_0 => self.pm1_0_ng_per_m3,
                Quantity::Pm2_5 => self.pm2_5_ng_per_m3,
                Quantity::Pm4_0 => self.pm4_0_ng_per_m3,
                Quantity::Pm10 => self.pm10_ng_per_m3,
                Quantity::VocIndex => self.voc_index_milli,
                Quantity::NoxIndex => self.nox_index_milli,
                Quantity::Illuminance => self.illuminance_millilux,
                Quantity::Proximity => self.proximity_counts,
            };
            unsigned.map(i64::from)
        }

        /// Returns the quantities that hold a value.
        #[must_use]
        pub fn quantities(&self) -> Quantities {
            Quantity::ALL
                .into_iter()
                .filter(|quantity| self.value(*quantity).is_some())
                .collect()
        }

        /// Fills each quantity this reading lacks from `other`; a value
        /// this reading holds is kept. A sensor that composes several chips
        /// merges their readings in priority order this way.
        #[must_use]
        pub fn or(self, other: Self) -> Self {
            Self {
                temperature_millicelsius: self
                    .temperature_millicelsius
                    .or(other.temperature_millicelsius),
                relative_humidity_millipercent: self
                    .relative_humidity_millipercent
                    .or(other.relative_humidity_millipercent),
                co2_ppm: self.co2_ppm.or(other.co2_ppm),
                pm1_0_ng_per_m3: self.pm1_0_ng_per_m3.or(other.pm1_0_ng_per_m3),
                pm2_5_ng_per_m3: self.pm2_5_ng_per_m3.or(other.pm2_5_ng_per_m3),
                pm4_0_ng_per_m3: self.pm4_0_ng_per_m3.or(other.pm4_0_ng_per_m3),
                pm10_ng_per_m3: self.pm10_ng_per_m3.or(other.pm10_ng_per_m3),
                voc_index_milli: self.voc_index_milli.or(other.voc_index_milli),
                nox_index_milli: self.nox_index_milli.or(other.nox_index_milli),
                illuminance_millilux: self.illuminance_millilux.or(other.illuminance_millilux),
                proximity_counts: self.proximity_counts.or(other.proximity_counts),
            }
        }
    }

    /// Semantic ambient-environment sensor peripheral.
    pub trait EnvironmentSensor {
        /// Concrete transport or measurement failure.
        type Error: core::fmt::Debug;

        /// Returns the quantities this sensor reports; fixed once initialized.
        fn quantities(&self) -> Quantities;

        /// Reads the latest value of every reported quantity. The future
        /// never waits for a gas index or a CO2 sensor to warm up: such a
        /// quantity is `None` until its chip has a valid value.
        fn read(&mut self) -> impl Future<Output = Result<EnvironmentReading, Self::Error>> + '_;
    }

    /// Move-only access to the Board's attached environment sensor.
    pub trait EnvironmentSensorPeripheral {
        /// Concrete, statically dispatched environment sensor.
        type EnvironmentSensor: EnvironmentSensor;

        /// Transfers the claimed environment sensor `instance` to its Plugin
        /// once; `None` when the Board has no such device or it was already
        /// taken.
        fn take_environment_sensor(
            &mut self,
            instance: crate::Instance,
        ) -> Option<Self::EnvironmentSensor>;
    }
}
