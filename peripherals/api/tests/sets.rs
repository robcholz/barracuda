//! Device sets forward every call of their kind's trait to the device they
//! hold and name its failure by the device.

#![allow(clippy::panic)]

use barracuda_peripheral::{
    audio::{AudioCodec, AudioDescriptor},
    buttons::{ButtonSnapshot, Buttons},
    buzzer::{Buzzer, Note, Volume},
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame},
    environment::{EnvironmentReading, EnvironmentSensor, Quantities, Quantity},
    imu::{Imu, ImuDescriptor, ImuSample, Vector3},
    indicator::Indicator,
    infrared::{Carrier, Infrared, NecCommand, Received},
    keyboard::{Key, KeyEvent, KeyState, Keyboard, Modifiers},
    led_strip::{LedStrip, Rgb8},
    power::{BatteryState, PowerMeasurement, PowerMonitor},
    real_time_clock::{DateTime, RealTimeClock},
    rotary_encoder::RotaryEncoder,
    touch::{Touch, TouchDescriptor, TouchFrame},
    vibration::{Pulse, Strength, Vibration},
};

/// Polls a future that never waits.
fn ready<F: core::future::Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    let mut context = core::task::Context::from_waker(core::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        core::task::Poll::Ready(output) => output,
        core::task::Poll::Pending => panic!("the test device never waits"),
    }
}

/// A device that works; `level` tells two of them apart.
struct Good(u8);

/// A device whose every fallible call fails.
struct Broken;

#[derive(Debug, PartialEq, Eq)]
struct Failure;

const DATE: DateTime = DateTime {
    year: 2026,
    month: 10,
    day: 22,
    weekday: 4,
    hour: 12,
    minute: 0,
    second: 0,
};

impl PowerMonitor for Good {
    type Error = core::convert::Infallible;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        Ok(PowerMeasurement {
            bus_microvolts: u64::from(self.0),
            shunt_nanovolts: 0,
            current_microamps: 0,
            power_microwatts: 0,
        })
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        Ok(Some(BatteryState {
            percent: self.0,
            charging: false,
        }))
    }
}

impl PowerMonitor for Broken {
    type Error = Failure;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::power_monitor_set! {
    pub enum PowerMonitors, PowerMonitorsError { Main(Good), Spare(Broken) }
}

#[test]
fn a_power_monitor_set_forwards_overridden_defaults() {
    let mut main = PowerMonitors::Main(Good(7));
    assert_eq!(
        main.measure().map(|sample| sample.bus_microvolts).ok(),
        Some(7)
    );
    assert_eq!(
        main.battery().ok().flatten().map(|battery| battery.percent),
        Some(7)
    );
    let mut spare = PowerMonitors::Spare(Broken);
    assert!(matches!(
        spare.measure(),
        Err(PowerMonitorsError::Spare(Failure))
    ));
    assert!(matches!(spare.battery(), Ok(None)));
}

impl RealTimeClock for Good {
    type Error = core::convert::Infallible;

    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 == 0)
    }

    fn read_datetime(&mut self) -> Result<DateTime, Self::Error> {
        Ok(DATE)
    }

    fn set_datetime(&mut self, datetime: DateTime) -> Result<(), Self::Error> {
        self.0 = datetime.second;
        Ok(())
    }
}

impl RealTimeClock for Broken {
    type Error = Failure;

    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        Err(Failure)
    }

    fn read_datetime(&mut self) -> Result<DateTime, Self::Error> {
        Err(Failure)
    }

    fn set_datetime(&mut self, _datetime: DateTime) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::real_time_clock_set! {
    pub enum Clocks, ClocksError { Main(Good), Backup(Broken) }
}

#[test]
fn a_real_time_clock_set_forwards_to_its_clock() {
    let mut main = Clocks::Main(Good(0));
    assert_eq!(main.read_datetime().ok(), Some(DATE));
    assert_eq!(main.lost_power().ok(), Some(true));
    assert!(main.set_datetime(DATE).is_ok());
    assert_eq!(main.lost_power().ok(), Some(true));
    assert!(matches!(
        Clocks::Backup(Broken).read_datetime(),
        Err(ClocksError::Backup(Failure))
    ));
}

impl Touch for Good {
    type Error = core::convert::Infallible;

    fn descriptor(&self) -> TouchDescriptor {
        TouchDescriptor {
            width: u16::from(self.0),
            height: 1,
            max_points: 1,
        }
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        Ok(TouchFrame::new())
    }
}

impl Touch for Broken {
    type Error = Failure;

    fn descriptor(&self) -> TouchDescriptor {
        TouchDescriptor {
            width: 0,
            height: 0,
            max_points: 0,
        }
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::touch_set! {
    pub enum Touches, TouchesError { Front(Good), Back(Broken) }
}

#[test]
fn a_touch_set_forwards_to_its_touch_input() {
    let mut front = Touches::Front(Good(9));
    assert_eq!(front.descriptor().width, 9);
    assert!(front.read_frame().is_ok_and(|frame| frame.is_empty()));
    assert!(matches!(
        Touches::Back(Broken).read_frame(),
        Err(TouchesError::Back(Failure))
    ));
}

impl Buttons for Good {
    type Error = core::convert::Infallible;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        Ok(ButtonSnapshot::new(&[self.0 != 0]))
    }
}

impl Buttons for Broken {
    type Error = Failure;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::buttons_set! {
    pub enum ButtonGroups, ButtonGroupsError { Front(Good), Side(Broken) }
}

#[test]
fn a_buttons_set_forwards_to_its_buttons() {
    assert_eq!(
        ButtonGroups::Front(Good(1))
            .read()
            .ok()
            .and_then(|snapshot| snapshot.is_pressed(0)),
        Some(true)
    );
    assert!(matches!(
        ButtonGroups::Side(Broken).read(),
        Err(ButtonGroupsError::Side(Failure))
    ));
}

impl Indicator for Good {
    type Error = core::convert::Infallible;

    fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        self.0 = u8::from(enabled);
        Ok(())
    }

    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 != 0)
    }
}

impl Indicator for Broken {
    type Error = Failure;

    fn set_enabled(&mut self, _enabled: bool) -> Result<(), Self::Error> {
        Err(Failure)
    }

    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::indicator_set! {
    pub enum Indicators, IndicatorsError { Power(Good), Status(Broken) }
}

#[test]
fn an_indicator_set_forwards_to_its_indicator() {
    let mut power = Indicators::Power(Good(0));
    assert!(power.on().is_ok());
    assert_eq!(power.is_enabled().ok(), Some(true));
    assert!(matches!(
        Indicators::Status(Broken).off(),
        Err(IndicatorsError::Status(Failure))
    ));
}

impl LedStrip for Good {
    type Error = core::convert::Infallible;

    fn len(&self) -> usize {
        usize::from(self.0)
    }

    fn write(&mut self, _pixels: &[Rgb8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl LedStrip for Broken {
    type Error = Failure;

    fn len(&self) -> usize {
        0
    }

    fn write(&mut self, _pixels: &[Rgb8]) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::led_strip_set! {
    pub enum Strips, StripsError { Ring(Good), Bar(Broken) }
}

#[test]
fn an_led_strip_set_forwards_to_its_strip() {
    let mut ring = Strips::Ring(Good(12));
    assert_eq!(ring.len(), 12);
    assert!(!ring.is_empty());
    assert!(ring.write(&[Rgb8::new(1, 2, 3)]).is_ok());
    let mut bar = Strips::Bar(Broken);
    assert!(bar.is_empty());
    assert!(matches!(bar.clear(), Err(StripsError::Bar(Failure))));
}

impl Buzzer for Good {
    type Error = core::convert::Infallible;

    fn frequency_range(&self) -> (u32, u32) {
        (u32::from(self.0), 20_000)
    }

    fn start(&mut self, _frequency_hz: u32, volume: Volume) -> Result<(), Self::Error> {
        self.0 = volume.get();
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn play_note(&mut self, note: Note, volume: Volume) -> Result<(), Self::Error> {
        self.start(note.frequency_hz(), volume)
    }
}

impl Buzzer for Broken {
    type Error = Failure;

    fn frequency_range(&self) -> (u32, u32) {
        (0, 0)
    }

    fn start(&mut self, _frequency_hz: u32, _volume: Volume) -> Result<(), Self::Error> {
        Err(Failure)
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn play_note(&mut self, _note: Note, _volume: Volume) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::buzzer_set! {
    pub enum Buzzers, BuzzersError { Main(Good), Spare(Broken) }
}

#[test]
fn a_buzzer_set_forwards_to_its_buzzer() {
    let mut main = Buzzers::Main(Good(20));
    assert_eq!(main.frequency_range(), (20, 20_000));
    assert!(ready(main.play(&[Note::new(440, 1)], Volume::percent(30))).is_ok());
    let Buzzers::Main(Good(volume)) = main else {
        panic!("the set keeps its buzzer");
    };
    assert_eq!(volume, 30);
    assert!(matches!(
        ready(Buzzers::Spare(Broken).play_note(Note::rest(1), Volume::MAX)),
        Err(BuzzersError::Spare(Failure))
    ));
}

impl Vibration for Good {
    type Error = core::convert::Infallible;

    fn has_variable_strength(&self) -> bool {
        self.0 != 0
    }

    fn set(&mut self, strength: Strength) -> Result<(), Self::Error> {
        self.0 = strength.get();
        Ok(())
    }

    async fn pulse(&mut self, _pattern: &[Pulse], strength: Strength) -> Result<(), Self::Error> {
        self.set(strength)
    }
}

impl Vibration for Broken {
    type Error = Failure;

    fn has_variable_strength(&self) -> bool {
        false
    }

    fn set(&mut self, _strength: Strength) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn pulse(&mut self, _pattern: &[Pulse], _strength: Strength) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::vibration_set! {
    pub enum Motors, MotorsError { Left(Good), Right(Broken) }
}

#[test]
fn a_vibration_set_forwards_to_its_motor() {
    let mut left = Motors::Left(Good(1));
    assert!(left.has_variable_strength());
    assert!(ready(left.pulse(&[Pulse::new(1, 1)], Strength::percent(40))).is_ok());
    assert!(matches!(
        Motors::Right(Broken).set(Strength::MAX),
        Err(MotorsError::Right(Failure))
    ));
}

impl Infrared for Good {
    type Error = core::convert::Infallible;

    fn can_receive(&self) -> bool {
        true
    }

    async fn send_nec(&mut self, command: NecCommand) -> Result<(), Self::Error> {
        self.0 = command.command();
        Ok(())
    }

    async fn send_raw(&mut self, _carrier: Carrier, timings_us: &[u16]) -> Result<(), Self::Error> {
        self.0 = u8::try_from(timings_us.len()).unwrap_or(u8::MAX);
        Ok(())
    }

    async fn receive(&mut self, timings_us: &mut [u16]) -> Result<Received, Self::Error> {
        if let Some(first) = timings_us.first_mut() {
            *first = u16::from(self.0);
        }
        Ok(Received::Raw { len: 1 })
    }
}

impl Infrared for Broken {
    type Error = Failure;

    fn can_receive(&self) -> bool {
        false
    }

    async fn send_nec(&mut self, _command: NecCommand) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn send_raw(
        &mut self,
        _carrier: Carrier,
        _timings_us: &[u16],
    ) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn receive(&mut self, _timings_us: &mut [u16]) -> Result<Received, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::infrared_set! {
    pub enum Remotes, RemotesError { Front(Good), Back(Broken) }
}

#[test]
fn an_infrared_set_forwards_to_its_device() {
    let mut front = Remotes::Front(Good(0));
    assert!(front.can_receive());
    assert!(ready(front.send_nec(NecCommand::new(1, 42))).is_ok());
    let mut timings = [0_u16; 2];
    assert_eq!(
        ready(front.receive(&mut timings)).ok(),
        Some(Received::Raw { len: 1 })
    );
    assert_eq!(timings[0], 42);
    let mut back = Remotes::Back(Broken);
    assert!(!back.can_receive());
    assert!(matches!(
        ready(back.send_raw(Carrier::NEC, &[1, 2])),
        Err(RemotesError::Back(Failure))
    ));
}

const STILL: CameraDescriptor = CameraDescriptor::new(2, 2, CameraPixelFormat::Grayscale);

impl Camera for Good {
    type Error = core::convert::Infallible;

    fn descriptor(&self) -> CameraDescriptor {
        STILL
    }

    async fn capture<'a>(&'a mut self, buffer: &'a mut [u8]) -> Result<CapturedFrame, Self::Error> {
        buffer.fill(self.0);
        Ok(CapturedFrame::new(buffer.len(), STILL))
    }
}

impl Camera for Broken {
    type Error = Failure;

    fn descriptor(&self) -> CameraDescriptor {
        STILL
    }

    async fn capture<'a>(
        &'a mut self,
        _buffer: &'a mut [u8],
    ) -> Result<CapturedFrame, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::camera_set! {
    pub enum Cameras, CamerasError { Front(Good), Rear(Broken) }
}

#[test]
fn a_camera_set_forwards_to_its_camera() {
    let mut front = Cameras::Front(Good(5));
    assert_eq!(front.descriptor(), STILL);
    let mut frame = [0_u8; 4];
    assert_eq!(
        ready(front.capture(&mut frame))
            .ok()
            .map(CapturedFrame::bytes_used),
        Some(4)
    );
    assert_eq!(frame, [5; 4]);
    assert!(matches!(
        ready(Cameras::Rear(Broken).capture(&mut frame)),
        Err(CamerasError::Rear(Failure))
    ));
}

impl AudioCodec for Good {
    type Error = core::convert::Infallible;

    fn descriptor(&self) -> AudioDescriptor {
        AudioDescriptor::new(16_000, self.0, 16)
    }

    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        self.0 = volume;
        Ok(())
    }

    async fn write<'a>(&'a mut self, _samples: &'a [i16]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn read<'a>(&'a mut self, samples: &'a mut [i16]) -> Result<(), Self::Error> {
        samples.fill(i16::from(self.0));
        Ok(())
    }
}

impl AudioCodec for Broken {
    type Error = Failure;

    fn descriptor(&self) -> AudioDescriptor {
        AudioDescriptor::new(0, 0, 0)
    }

    fn set_output_volume(&mut self, _volume: u8) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn write<'a>(&'a mut self, _samples: &'a [i16]) -> Result<(), Self::Error> {
        Err(Failure)
    }

    async fn read<'a>(&'a mut self, _samples: &'a mut [i16]) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::audio_codec_set! {
    pub enum Codecs, CodecsError { Speaker(Good), Headset(Broken) }
}

#[test]
fn an_audio_codec_set_forwards_to_its_codec() {
    let mut speaker = Codecs::Speaker(Good(2));
    assert_eq!(speaker.descriptor().channels(), 2);
    assert!(speaker.set_output_volume(3).is_ok());
    let mut samples = [0_i16; 2];
    assert!(ready(speaker.read(&mut samples)).is_ok());
    assert_eq!(samples, [3, 3]);
    assert!(matches!(
        ready(Codecs::Headset(Broken).write(&samples)),
        Err(CodecsError::Headset(Failure))
    ));
}

impl Keyboard for Good {
    type Error = core::convert::Infallible;

    fn poll_event(&mut self) -> Result<Option<KeyEvent>, Self::Error> {
        Ok(Some(KeyEvent::new(
            Key::Enter,
            KeyState::Pressed,
            Modifiers::empty(),
            None,
            u16::from(self.0),
        )))
    }
}

impl Keyboard for Broken {
    type Error = Failure;

    fn poll_event(&mut self) -> Result<Option<KeyEvent>, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::keyboard_set! {
    pub enum Keyboards, KeyboardsError { Built(Good), Cardputer(Broken) }
}

#[test]
fn a_keyboard_set_forwards_to_its_keyboard() {
    assert_eq!(
        Keyboards::Built(Good(8))
            .poll_event()
            .ok()
            .flatten()
            .map(KeyEvent::code),
        Some(8)
    );
    assert!(matches!(
        Keyboards::Cardputer(Broken).poll_event(),
        Err(KeyboardsError::Cardputer(Failure))
    ));
}

impl RotaryEncoder for Good {
    type Error = core::convert::Infallible;

    fn position(&mut self) -> Result<i64, Self::Error> {
        Ok(i64::from(self.0))
    }

    fn delta(&mut self) -> Result<i64, Self::Error> {
        Ok(1)
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.0 = 0;
        Ok(())
    }
}

impl RotaryEncoder for Broken {
    type Error = Failure;

    fn position(&mut self) -> Result<i64, Self::Error> {
        Err(Failure)
    }

    fn delta(&mut self) -> Result<i64, Self::Error> {
        Err(Failure)
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::rotary_encoder_set! {
    pub enum Knobs, KnobsError { Dial(Good), Side(Broken) }
}

#[test]
fn a_rotary_encoder_set_forwards_to_its_encoder() {
    let mut dial = Knobs::Dial(Good(4));
    assert_eq!(dial.position().ok(), Some(4));
    assert!(dial.reset().is_ok());
    assert_eq!(dial.position().ok(), Some(0));
    assert_eq!(dial.delta().ok(), Some(1));
    assert!(matches!(
        Knobs::Side(Broken).delta(),
        Err(KnobsError::Side(Failure))
    ));
}

impl Imu for Good {
    type Error = core::convert::Infallible;

    fn descriptor(&self) -> ImuDescriptor {
        ImuDescriptor::new(u32::from(self.0), 1, 1, 1, false)
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        let axis = i32::from(self.0);
        Ok(ImuSample::new(
            Vector3::new(axis, 0, 0),
            Vector3::default(),
            None,
        ))
    }
}

impl Imu for Broken {
    type Error = Failure;

    fn descriptor(&self) -> ImuDescriptor {
        ImuDescriptor::new(0, 0, 0, 0, false)
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::imu_set! {
    pub enum Imus, ImusError { Body(Good), Head(Broken) }
}

#[test]
fn an_imu_set_forwards_to_its_imu() {
    let mut body = Imus::Body(Good(3));
    assert_eq!(body.descriptor().accelerometer_rate_hz(), 3);
    assert_eq!(
        ready(body.read_sample())
            .ok()
            .map(|sample| sample.acceleration_mg().x),
        Some(3)
    );
    assert!(matches!(
        ready(Imus::Head(Broken).read_sample()),
        Err(ImusError::Head(Failure))
    ));
}

impl EnvironmentSensor for Good {
    type Error = core::convert::Infallible;

    fn quantities(&self) -> Quantities {
        Quantities::EMPTY.with(Quantity::Temperature)
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        Ok(EnvironmentReading {
            temperature_millicelsius: Some(i32::from(self.0)),
            ..EnvironmentReading::EMPTY
        })
    }
}

impl EnvironmentSensor for Broken {
    type Error = Failure;

    fn quantities(&self) -> Quantities {
        Quantities::EMPTY
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        Err(Failure)
    }
}

barracuda_peripheral::environment_sensor_set! {
    pub enum Sensors, SensorsError { Indoor(Good), Outdoor(Broken) }
}

#[test]
fn an_environment_sensor_set_forwards_to_its_sensor() {
    let mut indoor = Sensors::Indoor(Good(21));
    assert!(indoor.quantities().contains(Quantity::Temperature));
    assert_eq!(
        ready(indoor.read())
            .ok()
            .and_then(|reading| reading.temperature_millicelsius),
        Some(21)
    );
    assert!(matches!(
        ready(Sensors::Outdoor(Broken).read()),
        Err(SensorsError::Outdoor(Failure))
    ));
}
