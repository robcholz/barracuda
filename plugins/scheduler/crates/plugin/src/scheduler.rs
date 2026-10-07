use alloc::{boxed::Box, rc::Rc, string::String};
use core::{future::Future, pin::Pin};

use barracuda_plugin::manager::{
    PluginEntryIterator as _, PluginReadTransaction as _, PluginStorage, StorageError,
};
use barracuda_time_plugin::UtcClock;
use barracuda_workflow_plugin::{EmitError, Event, Topic, TopicError, WorkflowService};
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Timer};
use getset::CopyGetters;
use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time};

use crate::state::{PersistedSchedule, RecordError, ScheduleBook, ScheduleSpec};

const SCHEDULE_ID_MAX_BYTES: usize = 16;

/// Request for a new persistent schedule.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRequest {
    /// Stable schedule identifier and Workflow Event topic.
    pub id: String,
    /// One-shot or fixed-interval trigger policy.
    pub trigger: ScheduleTrigger,
}

/// Trigger policy accepted by [`Scheduler::schedule`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleTrigger {
    /// Trigger exactly once.
    Once {
        /// RFC3339 UTC timestamp with millisecond precision.
        at: String,
    },
    /// Trigger a fixed number of times at a fixed interval.
    Interval {
        /// RFC3339 UTC timestamp of the first occurrence.
        at: String,
        /// Seconds between occurrences.
        every_seconds: u64,
        /// Total number of occurrences, including the first.
        count: u32,
    },
}

impl ScheduleTrigger {
    fn parts(self) -> Result<(u64, u64, u32), ScheduleError> {
        match self {
            Self::Once { at } => Ok((parse_utc_seconds(&at)?, 0, 1)),
            Self::Interval {
                at,
                every_seconds,
                count,
            } if every_seconds != 0 && count != 0 => {
                Ok((parse_utc_seconds(&at)?, every_seconds, count))
            }
            Self::Interval { .. } => Err(ScheduleError::InvalidSchedule),
        }
    }
}

/// Request to cancel a live schedule.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    /// Stable schedule identifier.
    pub id: String,
}

/// Successful schedule creation.
#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct ScheduleAccepted {
    /// Accepted schedule identifier.
    pub id: String,
}

/// Successful schedule cancellation.
#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct ScheduleCancelled {
    /// Cancelled schedule identifier.
    pub id: String,
    /// Number of occurrences committed before cancellation.
    pub completed_runs: u64,
}

/// Business-level Scheduler rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScheduleError {
    /// Request fields do not describe a valid schedule.
    #[error("invalid schedule")]
    InvalidSchedule,
    /// A live schedule already uses the requested identifier.
    #[error("duplicate schedule identifier")]
    DuplicateId,
    /// No live schedule has the requested identifier.
    #[error("schedule not found")]
    NotFound,
    /// The UTC capability is not currently readable.
    #[error("UTC time unavailable")]
    TimeUnavailable,
    /// The requested first occurrence is already in the past.
    #[error("schedule trigger is in the past")]
    TriggerInPast,
    /// Persistent storage rejected the mutation.
    #[error("scheduler storage unavailable")]
    StorageUnavailable,
}

impl ScheduleError {
    /// Stable machine-readable code used by external adapters.
    #[must_use]
    pub const fn code(self) -> &'static str {
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

/// Scheduler timing policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct SchedulerConfig {
    /// Longest period Scheduler may sleep without re-reading the UTC clock.
    #[getset(get_copy = "pub")]
    max_recheck_millis: u64,
}

impl SchedulerConfig {
    /// Creates an explicit scheduler policy.
    #[must_use]
    pub const fn new(max_recheck_millis: u64) -> Self {
        Self { max_recheck_millis }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ScheduleId([u8; SCHEDULE_ID_MAX_BYTES]);

impl ScheduleId {
    pub(crate) fn new(value: &str) -> Result<Self, ScheduleError> {
        if value.is_empty()
            || value.len() > SCHEDULE_ID_MAX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(ScheduleError::InvalidSchedule);
        }
        let mut bytes = [0; SCHEDULE_ID_MAX_BYTES];
        bytes
            .get_mut(..value.len())
            .ok_or(ScheduleError::InvalidSchedule)?
            .copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }

    pub(crate) fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(SCHEDULE_ID_MAX_BYTES);
        core::str::from_utf8(self.0.get(..end).unwrap_or_default()).unwrap_or_default()
    }
}

type StorageFuture<'a> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + 'a>>;

trait SchedulerStorageBackend {
    fn write<'a>(&'a self, id: ScheduleId, value: PersistedSchedule) -> StorageFuture<'a>;
    fn delete<'a>(&'a self, id: ScheduleId) -> StorageFuture<'a>;
}

struct PluginSchedulerStorage<Storage>(Storage);

impl<Storage: PluginStorage> SchedulerStorageBackend for PluginSchedulerStorage<Storage> {
    fn write<'a>(&'a self, id: ScheduleId, value: PersistedSchedule) -> StorageFuture<'a> {
        Box::pin(async move { self.0.put(id.as_str(), &value).await })
    }

    fn delete<'a>(&'a self, id: ScheduleId) -> StorageFuture<'a> {
        Box::pin(async move { self.0.delete(id.as_str()).await })
    }
}

struct SchedulerShared {
    book: Mutex<NoopRawMutex, ScheduleBook>,
    changed: Signal<NoopRawMutex, ()>,
    storage: Box<dyn SchedulerStorageBackend>,
}

/// Persistent RTC-authoritative scheduling capability.
pub struct Scheduler {
    config: SchedulerConfig,
    shared: Rc<SchedulerShared>,
    clock: Rc<UtcClock>,
}

impl Scheduler {
    /// Restores Scheduler state from Plugin-scoped storage.
    pub async fn load<Storage: PluginStorage>(
        config: SchedulerConfig,
        clock: Rc<UtcClock>,
        storage: Storage,
    ) -> Result<Self, SchedulerStorageError> {
        let mut book = ScheduleBook::new();
        let transaction = storage.read_transaction().await;
        let mut entries = transaction.entries().await?;
        while let Some(entry) = entries.next().await? {
            let id = ScheduleId::new(entry.key())
                .map_err(|_error| SchedulerStorageError::InvalidState)?;
            let record = entry.value::<PersistedSchedule>()?;
            book.restore(id, record)?;
        }
        drop(entries);
        drop(transaction);
        Ok(Self {
            config,
            shared: Rc::new(SchedulerShared {
                book: Mutex::new(book),
                changed: Signal::new(),
                storage: Box::new(PluginSchedulerStorage(storage)),
            }),
            clock,
        })
    }

    /// Creates and durably stores one schedule.
    pub async fn schedule(
        &self,
        request: ScheduleRequest,
    ) -> Result<ScheduleAccepted, ScheduleError> {
        let id = ScheduleId::new(&request.id)?;
        let now_unix_seconds = self
            .clock
            .now()
            .map(u64::from)
            .map_err(|_error| ScheduleError::TimeUnavailable)?
            / 1_000;
        let (first_at_unix_seconds, every_seconds, count) = request.trigger.parts()?;
        if first_at_unix_seconds < now_unix_seconds {
            return Err(ScheduleError::TriggerInPast);
        }
        let mut book = self.shared.book.lock().await;
        book.add(ScheduleSpec {
            id,
            first_at_unix_seconds,
            every_seconds,
            count,
        })?;
        let Some(persisted) = book.persisted(&id) else {
            let _cancelled = book.cancel(&id);
            return Err(ScheduleError::StorageUnavailable);
        };
        if self.shared.storage.write(id, persisted).await.is_err() {
            let _cancelled = book.cancel(&id);
            return Err(ScheduleError::StorageUnavailable);
        }
        drop(book);
        self.shared.changed.signal(());
        Ok(ScheduleAccepted { id: request.id })
    }

    /// Cancels and removes one live schedule from persistent storage.
    pub async fn cancel(&self, request: CancelRequest) -> Result<ScheduleCancelled, ScheduleError> {
        let id = ScheduleId::new(&request.id)?;
        let mut book = self.shared.book.lock().await;
        let cancelled = book.cancel(&id)?;
        if self.shared.storage.delete(id).await.is_err() {
            let _restored = book.restore_cancelled(cancelled);
            return Err(ScheduleError::StorageUnavailable);
        }
        drop(book);
        self.shared.changed.signal(());
        Ok(ScheduleCancelled {
            id: request.id,
            completed_runs: cancelled.completed_runs(),
        })
    }

    /// Runs the due-occurrence loop and emits `scheduler.triggered` to Workflow.
    pub async fn run(&self, workflow: &WorkflowService) -> Result<(), SchedulerRunError> {
        loop {
            let Some(now_unix_seconds) = self.read_time() else {
                self.wait_for_change(Duration::from_millis(self.config.max_recheck_millis.max(1)))
                    .await;
                continue;
            };

            loop {
                let occurrence = self.shared.book.lock().await.take_due(now_unix_seconds);
                let Some(occurrence) = occurrence else {
                    break;
                };
                let topic = Topic::try_from(occurrence.id.as_str())?;
                log::info!(
                    "schedule `{}` triggered run {}",
                    occurrence.id.as_str(),
                    occurrence.run_number
                );
                workflow.emit_to::<SchedulerTriggered>(
                    topic,
                    serde_json::json!({
                        "id": occurrence.id.as_str(),
                        "run_number": occurrence.run_number,
                    }),
                )?;
                let mut book = self.shared.book.lock().await;
                if book.commit(occurrence, now_unix_seconds) {
                    let persisted = match book.persisted(&occurrence.id) {
                        Some(record) => self.shared.storage.write(occurrence.id, record).await,
                        None => self.shared.storage.delete(occurrence.id).await,
                    };
                    // The run is committed in memory, so other schedules keep
                    // firing; after a restart this one run may fire again.
                    if let Err(error) = persisted {
                        log::error!(
                            "failed to persist schedule `{}` after run {}: {}",
                            occurrence.id.as_str(),
                            occurrence.run_number,
                            SchedulerStorageError::from(error)
                        );
                    }
                }
            }

            let deadline = self.shared.book.lock().await.next_deadline();
            let Some(deadline) = deadline else {
                self.shared.changed.wait().await;
                continue;
            };
            let delay = deadline
                .saturating_sub(now_unix_seconds)
                .saturating_mul(1_000)
                .min(self.config.max_recheck_millis.max(1))
                .max(1);
            self.wait_for_change(Duration::from_millis(delay)).await;
        }
    }

    fn read_time(&self) -> Option<u64> {
        self.clock.now().ok().map(|now| u64::from(now) / 1_000)
    }

    async fn wait_for_change(&self, duration: Duration) {
        match select(Timer::after(duration), self.shared.changed.wait()).await {
            Either::First(()) | Either::Second(()) => {}
        }
    }
}

/// Event emitted once for every accepted schedule occurrence.
pub struct SchedulerTriggered;

impl Event for SchedulerTriggered {
    const ID: &'static str = "scheduler.triggered";
}

/// Failure loading or saving Scheduler persistent state.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerStorageError {
    /// Plugin-scoped key-value storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A stored record is incompatible.
    #[error("scheduler persistent state is invalid")]
    InvalidState,
}

impl From<RecordError> for SchedulerStorageError {
    fn from(_error: RecordError) -> Self {
        Self::InvalidState
    }
}

/// Failure of the Scheduler-owned occurrence loop.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerRunError {
    /// The schedule identifier could not be used as a Workflow topic.
    #[error(transparent)]
    Topic(#[from] TopicError),
    /// Workflow rejected the Event identity.
    #[error(transparent)]
    Emit(#[from] EmitError),
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
    u64::try_from(
        PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    )
    .map_err(|_error| ScheduleError::InvalidSchedule)
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

#[cfg(test)]
mod tests {
    use super::{ScheduleError, parse_utc_seconds};

    #[test]
    fn parses_only_the_existing_millisecond_utc_contract() {
        assert_eq!(
            parse_utc_seconds("2027-01-15T08:00:00.123Z"),
            Ok(1_800_000_000)
        );
        assert_eq!(
            parse_utc_seconds("2027-01-15T08:00:00Z"),
            Err(ScheduleError::InvalidSchedule)
        );
    }
}
