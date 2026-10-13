//! External implementation tests for the stable implementation API.

#![allow(clippy::expect_used, clippy::panic)]

use std::convert::Infallible;

use barracuda_peripheral::{
    Instance, PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPeripheral,
        DisplayPower, DisplayTechnology, PixelFormat, RefreshMode, RefreshRequest,
    },
    imu::{AxisTransform, Imu, ImuDescriptor, ImuPeripheral, ImuSample, SignedAxis, Vector3},
    indicator::Indicator,
    power::{PowerMeasurement, PowerMonitor, PowerMonitorPeripheral},
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::Rgb565,
    primitives::Rectangle,
};

#[derive(Default)]
struct CustomDisplay {
    drawn: std::vec::Vec<Rgb565>,
    flushed: bool,
    brightness: u8,
    power: Option<DisplayPower>,
    orientation: Option<DisplayOrientation>,
}

impl OriginDimensions for CustomDisplay {
    fn size(&self) -> Size {
        Size::new(320, 240)
    }
}

impl DrawTarget for CustomDisplay {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for _ in pixels {}
        Ok(())
    }
}

impl Display for CustomDisplay {
    type ControlError = Infallible;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BUFFERED
                | DisplayFeatures::PARTIAL_REFRESH
                | DisplayFeatures::BRIGHTNESS,
        )
    }

    fn draw_rgb565(
        &mut self,
        _area: Rectangle,
        pixels: &[Rgb565],
    ) -> Result<(), Self::RenderError> {
        self.drawn.extend_from_slice(pixels);
        Ok(())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        self.flushed = true;
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        self.power = Some(power);
        Ok(())
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.brightness = brightness;
        Ok(())
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        self.orientation = Some(orientation);
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

struct CustomDisplayImplementation;

impl PeripheralImplementation for CustomDisplayImplementation {
    type Bindings = ();
    type Config = u8;
    type Peripheral = CustomDisplay;
    type Error = Infallible;

    async fn initialize(
        _bindings: Self::Bindings,
        brightness: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ok(CustomDisplay {
            brightness,
            ..CustomDisplay::default()
        })
    }
}

struct TestPeripherals {
    display: Option<CustomDisplay>,
}

impl DisplayPeripheral for TestPeripherals {
    type Display = CustomDisplay;

    fn take_display(&mut self, instance: Instance) -> Option<Self::Display> {
        (instance.index() == 0)
            .then(|| self.display.take())
            .flatten()
    }
}

#[test]
fn custom_implementation_can_implement_the_stable_display_api() {
    fn assert_implementation<D: PeripheralImplementation<Peripheral = CustomDisplay>>() {}
    assert_implementation::<CustomDisplayImplementation>();
    let display = CustomDisplay {
        brightness: 127,
        ..CustomDisplay::default()
    };
    let mut peripherals = TestPeripherals {
        display: Some(display),
    };
    assert!(peripherals.take_display(Instance::new(1)).is_none());
    let mut display = peripherals
        .take_display(Instance::new(0))
        .expect("one display owner");
    assert!(peripherals.take_display(Instance::new(0)).is_none());

    let descriptor = display.descriptor();
    assert_eq!(descriptor.size(), Size::new(320, 240));
    assert_eq!(descriptor.pixel_format(), PixelFormat::Rgb565);
    assert_eq!(descriptor.technology(), DisplayTechnology::Lcd);
    assert!(descriptor.features().contains(DisplayFeatures::BUFFERED));
    assert!(
        descriptor
            .features()
            .contains(DisplayFeatures::PARTIAL_REFRESH)
    );
    assert_eq!(display.brightness, 127);

    let _flush = display.flush(RefreshRequest::new(None, RefreshMode::Automatic));
}

#[derive(Default)]
struct CustomIndicator(bool);

impl Indicator for CustomIndicator {
    type Error = Infallible;

    fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        self.0 = enabled;
        Ok(())
    }

    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0)
    }
}

#[test]
fn indicator_defaults_are_part_of_the_stable_api() {
    let mut indicator = CustomIndicator::default();
    indicator.on().expect("indicator on");
    assert!(indicator.is_enabled().expect("indicator state"));
    indicator.off().expect("indicator off");
    assert!(!indicator.is_enabled().expect("indicator state"));
}

struct CustomImu;

impl Imu for CustomImu {
    type Error = Infallible;

    fn descriptor(&self) -> ImuDescriptor {
        ImuDescriptor::new(100, 100, 4_000, 500_000, true)
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        Ok(ImuSample::new(
            Vector3::new(1, 2, 3),
            Vector3::new(4, 5, 6),
            Some(25_000),
        ))
    }
}

struct ImuPeripherals(Option<CustomImu>);

impl ImuPeripheral for ImuPeripherals {
    type Imu = CustomImu;

    fn take_imu(&mut self, _instance: Instance) -> Option<Self::Imu> {
        self.0.take()
    }
}

#[test]
fn custom_implementation_can_implement_the_stable_imu_api() {
    let transform = AxisTransform::new(
        SignedAxis::NegativeY,
        SignedAxis::PositiveX,
        SignedAxis::NegativeZ,
    );
    assert_eq!(
        transform.apply(Vector3::new(10, 20, 30)),
        Vector3::new(-20, 10, -30)
    );
    assert_eq!(transform.apply(Vector3::new(0, i32::MIN, 0)).x, i32::MAX);

    let mut peripherals = ImuPeripherals(Some(CustomImu));
    let mut imu = peripherals
        .take_imu(Instance::new(0))
        .expect("one IMU owner");
    assert!(peripherals.take_imu(Instance::new(0)).is_none());
    assert_eq!(imu.descriptor().accelerometer_range_mg(), 4_000);
    let _sample = imu.read_sample();
}

struct CustomPowerMonitor;

impl PowerMonitor for CustomPowerMonitor {
    type Error = Infallible;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        Ok(PowerMeasurement {
            bus_microvolts: 5_000_000,
            shunt_nanovolts: 2_500,
            current_microamps: 500,
            power_microwatts: 2_500,
        })
    }
}

struct PowerPeripherals(Option<CustomPowerMonitor>);

impl PowerMonitorPeripheral for PowerPeripherals {
    type PowerMonitor = CustomPowerMonitor;

    fn take_power_monitor(&mut self, _instance: Instance) -> Option<Self::PowerMonitor> {
        self.0.take()
    }
}

#[test]
fn custom_implementation_can_implement_the_stable_power_monitor_api() {
    let mut peripherals = PowerPeripherals(Some(CustomPowerMonitor));
    let mut monitor = peripherals
        .take_power_monitor(Instance::new(0))
        .expect("one power monitor owner");
    assert!(peripherals.take_power_monitor(Instance::new(0)).is_none());
    let sample = monitor.measure().expect("power sample");
    assert_eq!(sample.bus_microvolts, 5_000_000);
    assert_eq!(sample.current_microamps, 500);
}

barracuda_peripheral::display_set! {
    pub enum TwoDisplays, TwoDisplaysControlError, TwoDisplaysRenderError {
        Front(CustomDisplay),
        Back(CustomDisplay),
    }
}

fn ready<F: core::future::Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    let mut context = core::task::Context::from_waker(core::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        core::task::Poll::Ready(output) => output,
        core::task::Poll::Pending => panic!("the test display never waits"),
    }
}

#[test]
fn a_display_set_forwards_to_the_display_it_holds() {
    let mut displays = [
        TwoDisplays::Front(CustomDisplay::default()),
        TwoDisplays::Back(CustomDisplay {
            brightness: 9,
            ..CustomDisplay::default()
        }),
    ];
    for display in &mut displays {
        assert_eq!(display.size(), Size::new(320, 240));
        assert_eq!(display.descriptor().pixel_format(), PixelFormat::Rgb565);
        // Portable RGB565 pixels reach displays of any native color type.
        display
            .draw_iter([Pixel(
                embedded_graphics_core::geometry::Point::new(1, 2),
                Rgb565::new(1, 2, 3),
            )])
            .expect("draw");
        ready(display.set_brightness(200)).expect("brightness");
        ready(display.flush(RefreshRequest::new(None, RefreshMode::Full))).expect("flush");
    }
    let [TwoDisplays::Front(front), TwoDisplays::Back(back)] = displays else {
        panic!("variants keep their displays");
    };
    assert!(front.flushed && back.flushed);
    assert_eq!(front.drawn, [Rgb565::new(1, 2, 3)]);
    assert_eq!(back.drawn, [Rgb565::new(1, 2, 3)]);
    assert_eq!((front.brightness, back.brightness), (200, 200));
}
