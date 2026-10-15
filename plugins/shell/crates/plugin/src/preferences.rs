//! The `Preferences` capability: the device settings that the control center
//! and the Settings system app both change.

use core::cell::Cell;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::{Deserialize, Serialize};

/// The dimmest brightness the person can pick, in percent.
pub const MIN_BRIGHTNESS: u8 = 10;
/// Screen timeouts the Settings app offers, in seconds; 0 keeps the screen on.
pub const SCREEN_TIMEOUTS: [u16; 5] = [15, 30, 60, 300, 0];

/// The values [`Preferences`] holds, as the shell saves them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreferenceValues {
    /// Panel brightness in percent, from [`MIN_BRIGHTNESS`] to 100.
    pub brightness: u8,
    /// Speaker volume in percent.
    pub volume: u8,
    /// Whether the speaker is muted.
    pub muted: bool,
    /// Whether the microphone may listen.
    pub microphone: bool,
    /// Do not disturb: notices arrive without marking the home screen.
    pub quiet: bool,
    /// Seconds without a touch before the screen turns off; 0 is never.
    pub screen_off: u16,
}

impl Default for PreferenceValues {
    fn default() -> Self {
        Self {
            brightness: 70,
            volume: 60,
            muted: false,
            microphone: true,
            quiet: false,
            screen_off: 30,
        }
    }
}

/// The device's settings; published by the `shell` Plugin.
///
/// The control center and the Settings system app change them; the system
/// UI applies brightness and the screen timeout to the display, and keeps
/// them across restarts.
pub struct Preferences {
    values: Cell<PreferenceValues>,
    changed: Signal<NoopRawMutex, ()>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self::new(PreferenceValues::default())
    }
}

impl Preferences {
    /// Preferences starting at `values`.
    #[must_use]
    pub const fn new(values: PreferenceValues) -> Self {
        Self {
            values: Cell::new(values),
            changed: Signal::new(),
        }
    }

    /// The current values.
    #[must_use]
    pub fn get(&self) -> PreferenceValues {
        self.values.get()
    }

    /// Changes the values with `change`; out-of-range values are clamped and
    /// an unoffered screen timeout is replaced by the nearest longer one.
    pub fn update(&self, change: impl FnOnce(&mut PreferenceValues)) {
        let before = self.values.get();
        let mut values = before;
        change(&mut values);
        values.brightness = values.brightness.clamp(MIN_BRIGHTNESS, 100);
        values.volume = values.volume.min(100);
        if !SCREEN_TIMEOUTS.contains(&values.screen_off) {
            values.screen_off = SCREEN_TIMEOUTS
                .iter()
                .copied()
                .find(|offered| *offered != 0 && *offered >= values.screen_off)
                .unwrap_or(0);
        }
        if values != before {
            self.values.set(values);
            self.changed.signal(());
        }
    }

    /// Waits until a value changes. One waiter, the system UI, is served.
    pub async fn changed(&self) {
        self.changed.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_are_clamped_and_signalled() {
        let preferences = Preferences::default();
        preferences.update(|values| {
            values.brightness = 0;
            values.volume = 200;
            values.screen_off = 45;
        });
        let values = preferences.get();
        assert_eq!(values.brightness, MIN_BRIGHTNESS);
        assert_eq!(values.volume, 100);
        assert_eq!(values.screen_off, 60);
        assert!(preferences.changed.signaled());
    }

    #[test]
    fn saved_values_fill_in_missing_fields() {
        let values: PreferenceValues =
            serde_json::from_str(r#"{"muted":true}"#).unwrap_or_default();
        assert!(values.muted);
        assert_eq!(values.brightness, PreferenceValues::default().brightness);
    }
}
