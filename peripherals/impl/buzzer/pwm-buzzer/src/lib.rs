//! Passive-buzzer implementation over a retunable PWM output.
//!
//! A tone retunes the output's timer to its frequency and sets the duty
//! cycle from the volume: a square wave is loudest at half duty, so full
//! volume is 50 % and silence is 0 %. A note future holds a guard that
//! silences the output when the future completes or is dropped.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::PwmFrequencyControl;
use barracuda_peripheral::{
    PeripheralImplementation,
    buzzer::{Buzzer, Note, Volume},
};
use embassy_time::Timer;

/// Board-owned buzzer configuration; the output carries everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PwmBuzzerConfig;

impl PwmBuzzerConfig {
    /// Creates the configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Output failure.
#[derive(Debug)]
pub enum PwmBuzzerError<OutputError> {
    /// The frequency is outside the output's range.
    UnsupportedFrequency,
    /// The PWM output failed.
    Output(OutputError),
}

/// Initialized passive buzzer.
pub struct PwmBuzzer<OUT> {
    output: OUT,
}

/// Static factory used by generated Board composition.
pub struct PwmBuzzerImplementation<OUT>(PhantomData<fn() -> OUT>);

impl<OUT> PeripheralImplementation for PwmBuzzerImplementation<OUT>
where
    OUT: PwmFrequencyControl + 'static,
{
    type Bindings = OUT;
    type Config = PwmBuzzerConfig;
    type Peripheral = PwmBuzzer<OUT>;
    type Error = OUT::Error;

    async fn initialize(
        output: OUT,
        _config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut output = output;
        output.set_duty_cycle_fully_off()?;
        Ok(PwmBuzzer { output })
    }
}

impl<OUT> PwmBuzzer<OUT> {
    /// Returns the PWM output to its owner.
    #[must_use]
    pub fn into_output(self) -> OUT {
        self.output
    }
}

/// Sounds `frequency_hz` at `volume` on `output`.
fn sound<OUT: PwmFrequencyControl>(
    output: &mut OUT,
    frequency_hz: u32,
    volume: Volume,
) -> Result<(), PwmBuzzerError<OUT::Error>> {
    let (lowest, highest) = output.frequency_range();
    if !(lowest..=highest).contains(&frequency_hz) {
        return Err(PwmBuzzerError::UnsupportedFrequency);
    }
    output
        .set_frequency_hz(frequency_hz)
        .map_err(PwmBuzzerError::Output)?;
    // Half duty at full volume.
    output
        .set_duty_cycle_fraction(u16::from(volume.get()), 200)
        .map_err(PwmBuzzerError::Output)
}

/// Silences its output when dropped.
struct Silence<'a, OUT: PwmFrequencyControl>(&'a mut OUT);

impl<OUT: PwmFrequencyControl> Drop for Silence<'_, OUT> {
    fn drop(&mut self) {
        // Best effort on cancellation; a completed note reports its error.
        let _ = self.0.set_duty_cycle_fully_off();
    }
}

impl<OUT> Buzzer for PwmBuzzer<OUT>
where
    OUT: PwmFrequencyControl,
{
    type Error = PwmBuzzerError<OUT::Error>;

    fn frequency_range(&self) -> (u32, u32) {
        self.output.frequency_range()
    }

    fn start(&mut self, frequency_hz: u32, volume: Volume) -> Result<(), Self::Error> {
        sound(&mut self.output, frequency_hz, volume)
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.output
            .set_duty_cycle_fully_off()
            .map_err(PwmBuzzerError::Output)
    }

    async fn play_note(&mut self, note: Note, volume: Volume) -> Result<(), Self::Error> {
        let silence = Silence(&mut self.output);
        if !note.is_rest() {
            sound(silence.0, note.frequency_hz(), volume)?;
        }
        Timer::after_millis(u64::from(note.duration_ms())).await;
        silence
            .0
            .set_duty_cycle_fully_off()
            .map_err(PwmBuzzerError::Output)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_board_hal::PwmFrequencyControl;
    use barracuda_peripheral::buzzer::{Buzzer, Note, Volume};
    use embassy_futures::block_on;
    use embedded_hal::pwm::{ErrorType, SetDutyCycle};

    use super::PwmBuzzer;

    #[derive(Default)]
    struct RecordingPwm {
        frequency: u32,
        duty: u16,
        duties: Vec<(u32, u16)>,
    }

    impl ErrorType for RecordingPwm {
        type Error = Infallible;
    }

    impl SetDutyCycle for RecordingPwm {
        fn max_duty_cycle(&self) -> u16 {
            1024
        }

        fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
            self.duty = duty;
            self.duties.push((self.frequency, duty));
            Ok(())
        }
    }

    impl PwmFrequencyControl for RecordingPwm {
        fn frequency_range(&self) -> (u32, u32) {
            (76, 78_125)
        }

        fn set_frequency_hz(&mut self, frequency_hz: u32) -> Result<(), Self::Error> {
            self.frequency = frequency_hz;
            Ok(())
        }
    }

    #[test]
    fn full_volume_is_half_duty_and_a_note_ends_silent() {
        let mut buzzer = PwmBuzzer {
            output: RecordingPwm::default(),
        };
        block_on(buzzer.play(&[Note::new(440, 1), Note::rest(1)], Volume::MAX)).expect("play");
        let duties = &buzzer.output.duties;
        assert_eq!(duties[0], (440, 512));
        assert_eq!(duties[1], (440, 0));
        assert_eq!(buzzer.output.duty, 0);
    }

    #[test]
    fn volume_scales_duty_and_range_is_enforced() {
        let mut buzzer = PwmBuzzer {
            output: RecordingPwm::default(),
        };
        buzzer.start(2_000, Volume::percent(50)).expect("start");
        assert_eq!(buzzer.output.duty, 256);
        assert!(buzzer.start(10, Volume::MAX).is_err());
        buzzer.stop().expect("stop");
        assert_eq!(buzzer.output.duty, 0);
    }

    #[test]
    fn dropping_a_note_silences_the_buzzer() {
        let mut buzzer = PwmBuzzer {
            output: RecordingPwm::default(),
        };
        {
            let note = buzzer.play_note(Note::new(1_000, 60_000), Volume::MAX);
            let mut note = core::pin::pin!(note);
            let waker = core::task::Waker::noop();
            let mut context = core::task::Context::from_waker(waker);
            assert!(note.as_mut().poll(&mut context).is_pending());
        }
        assert_eq!(buzzer.output.duty, 0);
        assert_eq!(buzzer.output.duties.len(), 2);
    }
}
