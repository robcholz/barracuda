use alloc::collections::BTreeMap;
use core::mem::size_of;

use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::scheduler::{ScheduleError, ScheduleId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScheduleSpec {
    pub(crate) id: ScheduleId,
    pub(crate) first_at_unix_seconds: u64,
    pub(crate) every_seconds: u64,
    pub(crate) count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DueOccurrence {
    pub(crate) id: ScheduleId,
    pub(crate) scheduled_at_unix_seconds: u64,
    pub(crate) run_number: u64,
}

#[derive(Clone, Copy, Debug)]
struct Task {
    spec: ScheduleSpec,
    next_at_unix_seconds: u64,
    completed_runs: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CancelledSchedule {
    task: Task,
}

impl CancelledSchedule {
    pub(crate) const fn completed_runs(self) -> u64 {
        self.task.completed_runs
    }
}

const RECORD_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum RecordError {
    #[error("scheduler record has an unsupported version")]
    UnsupportedVersion,
    #[error("scheduler record is invalid")]
    Invalid,
}

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
pub(crate) struct PersistedSchedule {
    version: u32,
    count: u32,
    next_at_unix_seconds: u64,
    every_seconds: u64,
    completed_runs: u64,
}

impl PersistedSchedule {
    const fn from_task(task: Task) -> Self {
        Self {
            version: RECORD_VERSION,
            count: task.spec.count,
            next_at_unix_seconds: task.next_at_unix_seconds,
            every_seconds: task.spec.every_seconds,
            completed_runs: task.completed_runs,
        }
    }

    fn into_task(self, id: ScheduleId) -> Result<Task, RecordError> {
        if self.version != RECORD_VERSION {
            return Err(RecordError::UnsupportedVersion);
        }
        validate_repeat(self.every_seconds, self.count).map_err(|_error| RecordError::Invalid)?;
        if self.completed_runs >= u64::from(self.count) {
            return Err(RecordError::Invalid);
        }
        Ok(Task {
            spec: ScheduleSpec {
                id,
                first_at_unix_seconds: self.next_at_unix_seconds,
                every_seconds: self.every_seconds,
                count: self.count,
            },
            next_at_unix_seconds: self.next_at_unix_seconds,
            completed_runs: self.completed_runs,
        })
    }
}

const _: () = assert!(size_of::<PersistedSchedule>() == 32);

pub(crate) struct ScheduleBook {
    tasks: BTreeMap<ScheduleId, Task>,
}

impl ScheduleBook {
    pub(crate) const fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
        }
    }

    pub(crate) fn restore(
        &mut self,
        id: ScheduleId,
        persisted: PersistedSchedule,
    ) -> Result<(), RecordError> {
        let task = persisted.into_task(id)?;
        if self.tasks.contains_key(&id) {
            return Err(RecordError::Invalid);
        }
        self.tasks.insert(id, task);
        Ok(())
    }

    pub(crate) fn add(&mut self, spec: ScheduleSpec) -> Result<(), ScheduleError> {
        validate_repeat(spec.every_seconds, spec.count)?;
        if self.tasks.contains_key(&spec.id) {
            return Err(ScheduleError::DuplicateId);
        }
        self.tasks.insert(
            spec.id,
            Task {
                spec,
                next_at_unix_seconds: spec.first_at_unix_seconds,
                completed_runs: 0,
            },
        );
        Ok(())
    }

    pub(crate) fn persisted(&self, id: &ScheduleId) -> Option<PersistedSchedule> {
        self.tasks
            .get(id)
            .copied()
            .map(PersistedSchedule::from_task)
    }

    pub(crate) fn cancel(&mut self, id: &ScheduleId) -> Result<CancelledSchedule, ScheduleError> {
        self.tasks
            .remove(id)
            .map(|task| CancelledSchedule { task })
            .ok_or(ScheduleError::NotFound)
    }

    pub(crate) fn restore_cancelled(
        &mut self,
        cancelled: CancelledSchedule,
    ) -> Result<(), RecordError> {
        let id = cancelled.task.spec.id;
        if self.tasks.contains_key(&id) {
            return Err(RecordError::Invalid);
        }
        self.tasks.insert(id, cancelled.task);
        Ok(())
    }

    pub(crate) fn take_due(&self, now_unix_seconds: u64) -> Option<DueOccurrence> {
        self.tasks
            .values()
            .filter(|task| task.next_at_unix_seconds <= now_unix_seconds)
            .min_by_key(|task| (task.next_at_unix_seconds, task.spec.id))
            .map(|task| DueOccurrence {
                id: task.spec.id,
                scheduled_at_unix_seconds: task.next_at_unix_seconds,
                run_number: task.completed_runs.saturating_add(1),
            })
    }

    pub(crate) fn commit(
        &mut self,
        occurrence: DueOccurrence,
        observed_at_unix_seconds: u64,
    ) -> bool {
        let Some(task) = self.tasks.get_mut(&occurrence.id) else {
            return false;
        };
        if task.next_at_unix_seconds != occurrence.scheduled_at_unix_seconds
            || task.completed_runs.saturating_add(1) != occurrence.run_number
        {
            return false;
        }
        task.completed_runs = occurrence.run_number;
        if task.completed_runs >= u64::from(task.spec.count) {
            self.tasks.remove(&occurrence.id);
            return true;
        }

        let interval = task.spec.every_seconds;
        let first_next = task.next_at_unix_seconds.saturating_add(interval);
        let next = if first_next > observed_at_unix_seconds {
            first_next
        } else {
            let behind = observed_at_unix_seconds.saturating_sub(first_next);
            let skipped = behind.checked_div(interval).unwrap_or_default();
            first_next.saturating_add(skipped.saturating_add(1).saturating_mul(interval))
        };
        if next == task.next_at_unix_seconds {
            self.tasks.remove(&occurrence.id);
        } else {
            task.next_at_unix_seconds = next;
        }
        true
    }

    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.tasks
            .values()
            .map(|task| task.next_at_unix_seconds)
            .min()
    }
}

fn validate_repeat(every_seconds: u64, count: u32) -> Result<(), ScheduleError> {
    if count == 0 || (every_seconds == 0 && count != 1) {
        return Err(ScheduleError::InvalidSchedule);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{ScheduleBook, ScheduleSpec};
    use crate::scheduler::ScheduleId;

    fn id(value: &str) -> ScheduleId {
        ScheduleId::new(value).expect("valid schedule id")
    }

    #[test]
    fn periodic_schedule_commits_exactly_the_configured_runs() {
        let mut book = ScheduleBook::new();
        book.add(ScheduleSpec {
            id: id("repeat"),
            first_at_unix_seconds: 100,
            every_seconds: 50,
            count: 3,
        })
        .expect("add schedule");
        for (now, run) in [(100, 1), (150, 2), (200, 3)] {
            let due = book.take_due(now).expect("due occurrence");
            assert_eq!(due.run_number, run);
            assert!(book.commit(due, now));
        }
        assert!(book.take_due(u64::MAX).is_none());
    }

    #[test]
    fn missed_intervals_coalesce_to_the_next_future_deadline() {
        let mut book = ScheduleBook::new();
        book.add(ScheduleSpec {
            id: id("late"),
            first_at_unix_seconds: 100,
            every_seconds: 50,
            count: 3,
        })
        .expect("add schedule");
        let due = book.take_due(1_000).expect("overdue occurrence");
        assert!(book.commit(due, 1_000));
        assert_eq!(book.next_deadline(), Some(1_050));
    }
}
