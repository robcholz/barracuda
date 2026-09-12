//! Calibrated two-channel capacitive-touch button implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::CapacitiveTouchChannels;
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons},
};
use embedded_hal::delay::DelayNs;

/// Relative press threshold expressed in basis points of the learned baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacitiveTouchButtonsConfig {
    threshold_basis_points: u16,
}

impl CapacitiveTouchButtonsConfig {
    /// Creates a calibrated-button configuration.
    #[must_use]
    pub const fn new(threshold_basis_points: u16) -> Self {
        Self {
            threshold_basis_points,
        }
    }
}

/// Move-only channels and initialization delay.
pub struct CapacitiveTouchButtonsBindings<CHANNELS, DELAY> {
    channels: CHANNELS,
    delay: DELAY,
}

impl<CHANNELS, DELAY> CapacitiveTouchButtonsBindings<CHANNELS, DELAY> {
    /// Combines the two sampled channels and delay source.
    #[must_use]
    pub const fn new(channels: CHANNELS, delay: DELAY) -> Self {
        Self { channels, delay }
    }
}

/// Initialized calibrated buttons.
pub struct CapacitiveTouchButtons<CHANNELS> {
    channels: CHANNELS,
    baseline: [u32; 2],
    threshold_basis_points: u16,
}

/// Static factory used by generated Board composition.
pub struct CapacitiveTouchButtonsImplementation<CHANNELS, DELAY>(
    PhantomData<fn() -> (CHANNELS, DELAY)>,
);

impl<CHANNELS, DELAY> PeripheralImplementation
    for CapacitiveTouchButtonsImplementation<CHANNELS, DELAY>
where
    CHANNELS: CapacitiveTouchChannels + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = CapacitiveTouchButtonsBindings<CHANNELS, DELAY>;
    type Config = CapacitiveTouchButtonsConfig;
    type Peripheral = CapacitiveTouchButtons<CHANNELS>;
    type Error = CHANNELS::Error;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        bindings.delay.delay_ms(100);
        let mut baseline = bindings.channels.read()?;
        for _ in 0..2 {
            bindings.delay.delay_ms(20);
            let sample = bindings.channels.read()?;
            baseline[0] = average(baseline[0], sample[0]);
            baseline[1] = average(baseline[1], sample[1]);
        }
        Ok(CapacitiveTouchButtons {
            channels: bindings.channels,
            baseline,
            threshold_basis_points: config.threshold_basis_points,
        })
    }
}

impl<CHANNELS: CapacitiveTouchChannels> Buttons for CapacitiveTouchButtons<CHANNELS> {
    type Error = CHANNELS::Error;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        let sample = self.channels.read()?;
        let pressed = [
            is_pressed(self.baseline[0], sample[0], self.threshold_basis_points),
            is_pressed(self.baseline[1], sample[1], self.threshold_basis_points),
        ];
        for index in 0..2 {
            if !pressed[index] {
                self.baseline[index] = tracking_average(self.baseline[index], sample[index]);
            }
        }
        Ok(ButtonSnapshot::new(&pressed))
    }
}

const fn average(left: u32, right: u32) -> u32 {
    left / 2 + right / 2 + (left % 2 + right % 2) / 2
}

const fn tracking_average(baseline: u32, sample: u32) -> u32 {
    baseline - baseline / 8 + sample / 8
}

fn is_pressed(baseline: u32, sample: u32, threshold_basis_points: u16) -> bool {
    if baseline == 0 {
        return false;
    }
    u64::from(baseline.abs_diff(sample)) * 10_000
        > u64::from(baseline) * u64::from(threshold_basis_points)
}
