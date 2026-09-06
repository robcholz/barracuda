use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin_manager::PluginStorage;
use barracuda_time_plugin::UtcClock;
use serde::Deserialize;
use time::{Date, Month, PrimitiveDateTime, Time};

use crate::{
    component::SchedulerControl,
    json::{ScheduleAccepted, ScheduleRejected},
    model::{ScheduleId, ScheduleSpec},
};

/// Creates one absolute UTC trigger schedule.
pub struct Schedule;

impl JsonRpcSchema for Schedule {
    const ADDRESS: &'static str = "scheduler.schedule";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("schedule", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("schedule", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 32;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleRequest<'a> {
    #[serde(borrow)]
    id: &'a str,
    #[serde(borrow)]
    trigger: TriggerRequest<'a>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TriggerRequest<'a> {
    Once {
        #[serde(borrow)]
        at: &'a str,
    },
    Interval {
        #[serde(borrow)]
        at: &'a str,
        every_seconds: u64,
        count: u32,
    },
}

impl TriggerRequest<'_> {
    fn parts(self) -> Result<(u64, u64, u32), ScheduleError> {
        match self {
            Self::Once { at } => Ok((parse_utc_seconds(at)?, 0, 1)),
            Self::Interval {
                at,
                every_seconds,
                count,
            } if every_seconds != 0 && count != 0 => {
                Ok((parse_utc_seconds(at)?, every_seconds, count))
            }
            Self::Interval { .. } => Err(ScheduleError::InvalidSchedule),
        }
    }
}

fn parse_utc_seconds(value: &str) -> Result<u64, ScheduleError> {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || bytes.get(19) != Some(&b'.')
        || bytes.get(23) != Some(&b'Z')
    {
        return Err(ScheduleError::InvalidSchedule);
    }
    let year = decimal(bytes.get(0..4).unwrap_or_default())?;
    let month = decimal(bytes.get(5..7).unwrap_or_default())?;
    let day = decimal(bytes.get(8..10).unwrap_or_default())?;
    let hour = decimal(bytes.get(11..13).unwrap_or_default())?;
    let minute = decimal(bytes.get(14..16).unwrap_or_default())?;
    let second = decimal(bytes.get(17..19).unwrap_or_default())?;
    let millisecond = decimal(bytes.get(20..23).unwrap_or_default())?;

    let month =
        Month::try_from(u8::try_from(month).map_err(|_error| ScheduleError::InvalidSchedule)?)
            .map_err(|_error| ScheduleError::InvalidSchedule)?;
    let date = Date::from_calendar_date(
        i32::try_from(year).map_err(|_error| ScheduleError::InvalidSchedule)?,
        month,
        u8::try_from(day).map_err(|_error| ScheduleError::InvalidSchedule)?,
    )
    .map_err(|_error| ScheduleError::InvalidSchedule)?;
    let time = Time::from_hms_milli(
        u8::try_from(hour).map_err(|_error| ScheduleError::InvalidSchedule)?,
        u8::try_from(minute).map_err(|_error| ScheduleError::InvalidSchedule)?,
        u8::try_from(second).map_err(|_error| ScheduleError::InvalidSchedule)?,
        u16::try_from(millisecond).map_err(|_error| ScheduleError::InvalidSchedule)?,
    )
    .map_err(|_error| ScheduleError::InvalidSchedule)?;
    let seconds = PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp();
    u64::try_from(seconds).map_err(|_error| ScheduleError::InvalidSchedule)
}

fn decimal(bytes: &[u8]) -> Result<u32, ScheduleError> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or(ScheduleError::InvalidSchedule)?;
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .ok_or(ScheduleError::InvalidSchedule)
    })
}

/// Business-level rejection returned by Scheduler mutation RPCs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScheduleError {
    InvalidSchedule,
    DuplicateId,
    NotFound,
    TimeUnavailable,
    TriggerInPast,
    StorageUnavailable,
}

impl ScheduleError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidSchedule => "invalid_schedule",
            Self::DuplicateId => "duplicate_id",
            Self::NotFound => "not_found",
            Self::TimeUnavailable => "time_unavailable",
            Self::TriggerInPast => "trigger_in_past",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

/// Builds the reusable JSON handler for [`Schedule`].
pub(crate) fn schedule_handler<Storage>(
    control: SchedulerControl<Storage>,
    clock: Rc<UtcClock>,
) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        let clock = Rc::clone(&clock);
        async move {
            let request = request.deserialize::<ScheduleRequest<'_>>()?;
            let result = schedule(&shared, &clock, request).await;
            match result {
                Ok(id) => response.write(&ScheduleAccepted::new(id)).await,
                Err(error) => response.write(&ScheduleRejected::new(error)).await,
            }
        }
    }
}

async fn schedule(
    shared: &crate::component::SchedulerShared<impl PluginStorage>,
    clock: &UtcClock,
    request: ScheduleRequest<'_>,
) -> Result<ScheduleId, ScheduleError> {
    let id = ScheduleId::new(request.id).map_err(|_error| ScheduleError::InvalidSchedule)?;
    let now_unix_seconds = u64::from(
        clock
            .now()
            .map_err(|_error| ScheduleError::TimeUnavailable)?,
    ) / 1_000;
    let (first_at_unix_seconds, every_seconds, count) = request.trigger.parts()?;
    if first_at_unix_seconds < now_unix_seconds {
        return Err(ScheduleError::TriggerInPast);
    }
    let mut book = shared.book.lock().await;
    book.add(ScheduleSpec::new(
        id,
        first_at_unix_seconds,
        every_seconds,
        count,
    ))?;
    let Some(persisted) = book.persisted(&id) else {
        let _cancelled = book.cancel(&id);
        return Err(ScheduleError::StorageUnavailable);
    };
    if shared.write(id, &persisted).await.is_err() {
        let _cancelled = book.cancel(&id);
        return Err(ScheduleError::StorageUnavailable);
    }
    drop(book);
    shared.changed.signal(());
    Ok(id)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{ScheduleError, TriggerRequest, parse_utc_seconds};

    #[test]
    fn parses_rfc3339_utc_at_second_precision() {
        assert_eq!(
            parse_utc_seconds("2027-01-15T08:00:00.123Z"),
            Ok(1_800_000_000)
        );
        assert_eq!(
            parse_utc_seconds("2027-01-15T08:00:00Z"),
            Err(ScheduleError::InvalidSchedule)
        );
        assert_eq!(
            parse_utc_seconds("2027-01-15T08:00:00.123+00:00"),
            Err(ScheduleError::InvalidSchedule)
        );
    }

    #[test]
    fn rejects_invalid_interval_shape() {
        let trigger = TriggerRequest::Interval {
            at: "2027-01-15T08:00:00.000Z",
            every_seconds: 0,
            count: 3,
        };
        assert_eq!(trigger.parts(), Err(ScheduleError::InvalidSchedule));
    }
}
