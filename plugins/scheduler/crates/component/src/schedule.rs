use alloc::rc::Rc;

use barracuda_event_router::{
    RpcContext, RpcFrame, RpcHandler, RpcMethod, Unary, rpc_dynamic, rpc_message,
};
use getset::CopyGetters;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};
use time::{Date, Month, PrimitiveDateTime, Time};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use barracuda_time_component::now::{Now, TimeNowRequest};

use crate::{
    ScheduleId, SchedulerControl,
    model::{ScheduleSpec, unix_seconds},
};

/// Absolute UTC calendar time at which a trigger begins.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct TriggerAt {
    /// UTC year.
    #[getset(get_copy = "pub")]
    year: u16,
    /// UTC month in the range 1 through 12.
    #[getset(get_copy = "pub")]
    month: u8,
    /// UTC day of month.
    #[getset(get_copy = "pub")]
    day: u8,
    /// UTC hour in the range 0 through 23.
    #[getset(get_copy = "pub")]
    hour: u8,
    /// UTC minute in the range 0 through 59.
    #[getset(get_copy = "pub")]
    minute: u8,
    /// UTC second in the range 0 through 59.
    #[getset(get_copy = "pub")]
    second: u8,
    #[serde(skip)]
    reserved: u8,
}

impl TriggerAt {
    /// Creates one UTC calendar value. The schedule RPC validates its ranges.
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            reserved: 0,
        }
    }

    fn unix_seconds(self) -> Result<u64, ScheduleError> {
        let month = Month::try_from(self.month).map_err(|_error| ScheduleError::InvalidSchedule)?;
        let date = Date::from_calendar_date(i32::from(self.year), month, self.day)
            .map_err(|_error| ScheduleError::InvalidSchedule)?;
        let time = Time::from_hms(self.hour, self.minute, self.second)
            .map_err(|_error| ScheduleError::InvalidSchedule)?;
        let timestamp = PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp();
        u64::try_from(timestamp).map_err(|_error| ScheduleError::InvalidSchedule)
    }
}

/// Kind of trigger rule stored by the Scheduler.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub enum TriggerKind {
    /// Trigger exactly once at `at`.
    Once = 0,
    /// Trigger first at `at`, then at a fixed interval.
    Interval = 1,
}

/// One-time or fixed-interval trigger rule.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes, CopyGetters,
)]
pub struct Trigger {
    /// Trigger rule kind.
    #[getset(get_copy = "pub")]
    kind: TriggerKind,
    reserved_before_at: u8,
    /// Absolute UTC time of the first trigger.
    #[getset(get_copy = "pub")]
    at: TriggerAt,
    reserved_before_interval: [u8; 6],
    /// Interval between triggers; zero for a one-time trigger.
    #[getset(get_copy = "pub")]
    every_seconds: u64,
    /// Total number of triggers, including the first.
    #[getset(get_copy = "pub")]
    count: u32,
    reserved: u32,
}

impl Trigger {
    /// Creates a one-time trigger.
    #[must_use]
    pub const fn once(at: TriggerAt) -> Self {
        Self {
            kind: TriggerKind::Once,
            reserved_before_at: 0,
            at,
            reserved_before_interval: [0; 6],
            every_seconds: 0,
            count: 1,
            reserved: 0,
        }
    }

    /// Creates an interval trigger whose `count` includes the first trigger.
    #[must_use]
    pub const fn interval(at: TriggerAt, every_seconds: u64, count: u32) -> Self {
        Self {
            kind: TriggerKind::Interval,
            reserved_before_at: 0,
            at,
            reserved_before_interval: [0; 6],
            every_seconds,
            count,
            reserved: 0,
        }
    }

    const fn validate(self) -> Result<(), ScheduleError> {
        match self.kind {
            TriggerKind::Once if self.every_seconds == 0 && self.count == 1 => Ok(()),
            TriggerKind::Interval if self.every_seconds != 0 && self.count != 0 => Ok(()),
            TriggerKind::Once | TriggerKind::Interval => Err(ScheduleError::InvalidSchedule),
        }
    }
}

impl Serialize for Trigger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.kind {
            TriggerKind::Once => {
                let mut state = serializer.serialize_struct("Trigger", 2)?;
                state.serialize_field("type", "once")?;
                state.serialize_field("at", &self.at)?;
                state.end()
            }
            TriggerKind::Interval => {
                let mut state = serializer.serialize_struct("Trigger", 4)?;
                state.serialize_field("type", "interval")?;
                state.serialize_field("at", &self.at)?;
                state.serialize_field("every_seconds", &self.every_seconds)?;
                state.serialize_field("count", &self.count)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum TriggerJson {
            Once {
                at: TriggerAt,
            },
            Interval {
                at: TriggerAt,
                every_seconds: u64,
                count: u32,
            },
        }

        Ok(match TriggerJson::deserialize(deserializer)? {
            TriggerJson::Once { at } => Self::once(at),
            TriggerJson::Interval {
                at,
                every_seconds,
                count,
            } => Self::interval(at, every_seconds, count),
        })
    }
}

/// Request accepted by [`Schedule`].
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct ScheduleRequest {
    /// Stable caller-selected schedule identifier.
    #[getset(get_copy = "pub")]
    id: ScheduleId,
    /// One-time or interval trigger rule.
    #[getset(get_copy = "pub")]
    trigger: Trigger,
}

impl ScheduleRequest {
    /// Creates one request.
    #[must_use]
    pub const fn new(id: ScheduleId, trigger: Trigger) -> Self {
        Self { id, trigger }
    }

    fn spec(self, now_unix_seconds: u64) -> Result<ScheduleSpec, ScheduleError> {
        self.trigger.validate()?;
        let first_at_unix_seconds = self.trigger.at.unix_seconds()?;
        if first_at_unix_seconds < now_unix_seconds {
            return Err(ScheduleError::TriggerInPast);
        }
        Ok(ScheduleSpec::new(
            self.id,
            first_at_unix_seconds,
            self.trigger.every_seconds,
            self.trigger.count,
        ))
    }
}

/// Accepted schedule identity and first deadline.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct ScheduleResponse {
    /// Accepted schedule identifier.
    #[getset(get_copy = "pub")]
    id: ScheduleId,
}

impl ScheduleResponse {
    const fn new(id: ScheduleId) -> Self {
        Self { id }
    }
}

/// Business-level rejection returned by scheduler mutation RPCs.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum ScheduleError {
    /// Calendar or trigger-rule fields do not describe a valid schedule.
    InvalidSchedule = 0,
    /// A live schedule already owns the requested identifier.
    DuplicateId = 1,
    /// The configured in-memory task capacity is full.
    CapacityExceeded = 2,
    /// No live schedule owns the requested identifier.
    NotFound = 3,
    /// `time.now` is unavailable or returned an invalid UTC calendar value.
    TimeUnavailable = 4,
    /// The requested `at` time is earlier than the current RTC value.
    TriggerInPast = 5,
}

/// Creates one absolute UTC trigger schedule.
pub struct Schedule;

#[rpc_dynamic]
impl RpcMethod for Schedule {
    const ADDRESS: &'static str = "scheduler.schedule";
    type Request = ScheduleRequest;
    type Response = ScheduleResponse;
    type Error = ScheduleError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Schedule`].
pub fn schedule_handler(control: SchedulerControl) -> impl RpcHandler<Schedule> {
    move |context: RpcContext, request: RpcFrame<ScheduleRequest>| {
        let result = request.view().copied();
        let shared = Rc::clone(&control.shared);
        async move {
            let request = result?;
            let call = match context.client().call::<Now>(TimeNowRequest::new()) {
                Ok(call) => call,
                Err(_error) => return Ok(Err(ScheduleError::TimeUnavailable)),
            };
            let outcome = match call.await {
                Ok(outcome) => outcome,
                Err(_error) => return Ok(Err(ScheduleError::TimeUnavailable)),
            };
            let frame = match outcome {
                Ok(frame) => frame,
                Err(_error) => return Ok(Err(ScheduleError::TimeUnavailable)),
            };
            let now_unix_seconds = match unix_seconds(*frame.view()?) {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            let spec = match request.spec(now_unix_seconds) {
                Ok(spec) => spec,
                Err(error) => return Ok(Err(error)),
            };
            if let Err(error) = shared.book.borrow_mut().add(spec) {
                return Ok(Err(error));
            }
            shared.changed.signal(());
            Ok(Ok(ScheduleResponse::new(request.id)))
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{ScheduleError, ScheduleRequest, Trigger, TriggerAt};
    use crate::ScheduleId;

    fn id() -> ScheduleId {
        ScheduleId::new("calendar-trigger").expect("valid schedule id")
    }

    #[test]
    fn rejects_at_before_current_rtc_value() {
        let request =
            ScheduleRequest::new(id(), Trigger::once(TriggerAt::new(2027, 1, 15, 8, 0, 0)));
        assert_eq!(
            request.spec(1_800_000_001),
            Err(ScheduleError::TriggerInPast)
        );
    }

    #[test]
    fn rejects_invalid_at_calendar_fields() {
        let request =
            ScheduleRequest::new(id(), Trigger::once(TriggerAt::new(2027, 13, 15, 8, 0, 0)));
        assert_eq!(request.spec(0), Err(ScheduleError::InvalidSchedule));
    }

    #[test]
    fn rejects_trigger_kind_and_interval_mismatch() {
        let mut trigger = Trigger::once(TriggerAt::new(2027, 1, 15, 8, 0, 0));
        trigger.every_seconds = 60;
        let request = ScheduleRequest::new(id(), trigger);
        assert_eq!(request.spec(0), Err(ScheduleError::InvalidSchedule));
    }
}
