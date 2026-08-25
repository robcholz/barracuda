use alloc::string::String;
use barracuda_time_component::now::TimeNow;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, Month, PrimitiveDateTime, Time};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::schedule::ScheduleError;

/// Maximum UTF-8 byte length of a schedule identifier.
pub const SCHEDULE_ID_MAX_BYTES: usize = 16;
// Keep the established scheduler RPC layout while reserving one NUL byte.
const SCHEDULE_ID_CAPACITY: usize = 32;

/// Stable fixed-capacity schedule identifier.
#[repr(transparent)]
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Immutable,
    IntoBytes,
    KnownLayout,
    Ord,
    PartialEq,
    PartialOrd,
    TryFromBytes,
)]
pub struct ScheduleId([u8; SCHEDULE_ID_CAPACITY]);

/// Failure constructing a [`ScheduleId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScheduleIdError {
    /// Identifier is empty.
    #[error("schedule id cannot be empty")]
    Empty,
    /// Identifier exceeds [`SCHEDULE_ID_MAX_BYTES`].
    #[error("schedule id exceeds its fixed capacity")]
    TooLong,
    /// Identifier contains a character outside ASCII letters, digits, `_`, `-`, and `.`.
    #[error("schedule id contains an invalid character")]
    InvalidCharacter,
}

impl ScheduleId {
    /// Validates and encodes one identifier.
    pub fn new(value: &str) -> Result<Self, ScheduleIdError> {
        if value.is_empty() {
            return Err(ScheduleIdError::Empty);
        }
        if value.len() > SCHEDULE_ID_MAX_BYTES {
            return Err(ScheduleIdError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(ScheduleIdError::InvalidCharacter);
        }
        let mut bytes = [0; SCHEDULE_ID_CAPACITY];
        bytes
            .get_mut(..value.len())
            .ok_or(ScheduleIdError::TooLong)?
            .copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(SCHEDULE_ID_CAPACITY);
        core::str::from_utf8(self.0.get(..end).unwrap_or_default()).unwrap_or_default()
    }
}

impl Serialize for ScheduleId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScheduleId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// Validated repeat configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScheduleConfig {
    every_seconds: u64,
    count: u32,
}

impl ScheduleConfig {
    /// Validates one-shot or periodic repeat semantics.
    pub const fn new(every_seconds: u64, count: u32) -> Result<Self, ScheduleError> {
        if count == 0 || (every_seconds == 0 && count != 1) {
            return Err(ScheduleError::InvalidSchedule);
        }
        Ok(Self {
            every_seconds,
            count,
        })
    }

    /// Interval in seconds; zero means a one-time trigger.
    #[must_use]
    pub const fn every_seconds(self) -> u64 {
        self.every_seconds
    }

    /// Total number of triggers, including the first.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.count
    }
}

/// Complete validated schedule stored by the Component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScheduleSpec {
    id: ScheduleId,
    first_at_unix_seconds: u64,
    repeat: ScheduleConfig,
}

impl ScheduleSpec {
    /// Creates a schedule description. Invalid repeat combinations are rejected by `add`.
    #[must_use]
    pub const fn new(
        id: ScheduleId,
        first_at_unix_seconds: u64,
        every_seconds: u64,
        count: u32,
    ) -> Self {
        Self {
            id,
            first_at_unix_seconds,
            repeat: ScheduleConfig {
                every_seconds,
                count,
            },
        }
    }

    /// Stable schedule identifier.
    #[must_use]
    pub const fn id(self) -> ScheduleId {
        self.id
    }

    /// First absolute RTC deadline.
    #[must_use]
    pub const fn first_at_unix_seconds(self) -> u64 {
        self.first_at_unix_seconds
    }

    /// Repeat configuration.
    #[must_use]
    pub const fn repeat(self) -> ScheduleConfig {
        self.repeat
    }
}

/// One occurrence selected for Event emission but not yet committed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DueOccurrence {
    id: ScheduleId,
    scheduled_at_unix_seconds: u64,
    run_number: u64,
}

impl DueOccurrence {
    pub(crate) const fn new(
        id: ScheduleId,
        scheduled_at_unix_seconds: u64,
        run_number: u64,
    ) -> Self {
        Self {
            id,
            scheduled_at_unix_seconds,
            run_number,
        }
    }

    /// Schedule identifier.
    #[must_use]
    pub const fn id(self) -> ScheduleId {
        self.id
    }

    /// Absolute RTC deadline represented by this occurrence.
    #[must_use]
    pub const fn scheduled_at_unix_seconds(self) -> u64 {
        self.scheduled_at_unix_seconds
    }

    /// One-based committed run number.
    #[must_use]
    pub const fn run_number(self) -> u64 {
        self.run_number
    }
}

/// Snapshot returned when a live schedule is cancelled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CancelledSchedule {
    completed_runs: u64,
}

impl CancelledSchedule {
    pub(crate) const fn new(completed_runs: u64) -> Self {
        Self { completed_runs }
    }

    /// Number of Events committed before cancellation.
    #[must_use]
    pub const fn completed_runs(self) -> u64 {
        self.completed_runs
    }
}

pub(crate) fn unix_seconds(value: TimeNow) -> Result<u64, ScheduleError> {
    let month = Month::try_from(value.month()).map_err(|_error| ScheduleError::TimeUnavailable)?;
    let date = Date::from_calendar_date(i32::from(value.year()), month, value.day())
        .map_err(|_error| ScheduleError::TimeUnavailable)?;
    let time = Time::from_hms(value.hour(), value.minute(), value.second())
        .map_err(|_error| ScheduleError::TimeUnavailable)?;
    let timestamp = PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp();
    u64::try_from(timestamp).map_err(|_error| ScheduleError::TimeUnavailable)
}
