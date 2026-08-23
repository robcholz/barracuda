use alloc::collections::BTreeMap;

use crate::{
    model::{CancelledSchedule, DueOccurrence, ScheduleConfig, ScheduleId, ScheduleSpec},
    schedule::ScheduleError,
};

#[derive(Clone, Copy, Debug)]
struct Task {
    spec: ScheduleSpec,
    next_at_unix_seconds: u64,
    completed_runs: u64,
}

/// Bounded in-memory schedule collection with explicit Event commit semantics.
pub(crate) struct ScheduleBook {
    capacity: usize,
    tasks: BTreeMap<ScheduleId, Task>,
}

impl ScheduleBook {
    /// Creates an empty collection. Capacity zero rejects every add.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tasks: BTreeMap::new(),
        }
    }

    /// Adds a validated schedule.
    pub fn add(&mut self, spec: ScheduleSpec) -> Result<(), ScheduleError> {
        let _repeat = ScheduleConfig::new(spec.repeat().every_seconds(), spec.repeat().count())?;
        if self.tasks.contains_key(&spec.id()) {
            return Err(ScheduleError::DuplicateId);
        }
        if self.tasks.len() >= self.capacity {
            return Err(ScheduleError::CapacityExceeded);
        }
        self.tasks.insert(
            spec.id(),
            Task {
                spec,
                next_at_unix_seconds: spec.first_at_unix_seconds(),
                completed_runs: 0,
            },
        );
        Ok(())
    }

    /// Removes one live schedule.
    pub fn cancel(&mut self, id: &ScheduleId) -> Result<CancelledSchedule, ScheduleError> {
        self.tasks
            .remove(id)
            .map(|task| CancelledSchedule::new(task.completed_runs))
            .ok_or(ScheduleError::NotFound)
    }

    /// Returns the earliest occurrence due according to authoritative RTC time.
    #[must_use]
    pub fn take_due(&self, now_unix_seconds: u64) -> Option<DueOccurrence> {
        self.tasks
            .values()
            .filter(|task| task.next_at_unix_seconds <= now_unix_seconds)
            .min_by_key(|task| (task.next_at_unix_seconds, task.spec.id()))
            .map(|task| {
                DueOccurrence::new(
                    task.spec.id(),
                    task.next_at_unix_seconds,
                    task.completed_runs.saturating_add(1),
                )
            })
    }

    /// Commits an occurrence after Event Router accepted its Event.
    pub fn commit(&mut self, occurrence: DueOccurrence, observed_at_unix_seconds: u64) {
        let Some(task) = self.tasks.get_mut(&occurrence.id()) else {
            return;
        };
        if task.next_at_unix_seconds != occurrence.scheduled_at_unix_seconds()
            || task.completed_runs.saturating_add(1) != occurrence.run_number()
        {
            return;
        }
        task.completed_runs = occurrence.run_number();
        let repeat = task.spec.repeat();
        let exhausted = task.completed_runs >= u64::from(repeat.count());
        if exhausted {
            self.tasks.remove(&occurrence.id());
            return;
        }

        let interval = repeat.every_seconds();
        let first_next = task.next_at_unix_seconds.saturating_add(interval);
        let next = if first_next > observed_at_unix_seconds {
            first_next
        } else {
            let behind = observed_at_unix_seconds.saturating_sub(first_next);
            let skipped = behind.checked_div(interval).unwrap_or_default();
            first_next.saturating_add(skipped.saturating_add(1).saturating_mul(interval))
        };
        if next == task.next_at_unix_seconds {
            self.tasks.remove(&occurrence.id());
        } else {
            task.next_at_unix_seconds = next;
        }
    }

    /// Earliest absolute RTC deadline across live schedules.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.tasks
            .values()
            .map(|task| task.next_at_unix_seconds)
            .min()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::ScheduleBook;
    use crate::model::{ScheduleConfig, ScheduleId, ScheduleSpec};

    fn id(value: &str) -> ScheduleId {
        ScheduleId::new(value).expect("valid schedule id")
    }

    #[test]
    fn one_shot_fires_once_at_absolute_rtc_deadline() {
        let mut book = ScheduleBook::new(4);
        book.add(ScheduleSpec::new(id("once"), 10_000, 0, 1))
            .expect("add schedule");
        assert!(book.take_due(9_999).is_none());
        let due = book.take_due(10_000).expect("due occurrence");
        assert_eq!(due.id().as_str(), "once");
        assert_eq!(due.run_number(), 1);
        assert_eq!(due.scheduled_at_unix_seconds(), 10_000);
        book.commit(due, 10_000);
        assert!(book.take_due(u64::MAX).is_none());
    }

    #[test]
    fn periodic_schedule_repeats_exact_number_of_runs() {
        let mut book = ScheduleBook::new(4);
        book.add(ScheduleSpec::new(id("repeat"), 100, 50, 3))
            .expect("add schedule");
        for (now, run) in [(100, 1), (150, 2), (200, 3)] {
            let due = book.take_due(now).expect("due occurrence");
            assert_eq!(due.run_number(), run);
            book.commit(due, now);
        }
        assert!(book.take_due(u64::MAX).is_none());
    }

    #[test]
    fn missed_intervals_coalesce_without_losing_run_budget() {
        let mut book = ScheduleBook::new(4);
        book.add(ScheduleSpec::new(id("coalesce"), 100, 50, 3))
            .expect("add schedule");
        let first = book.take_due(1_000).expect("overdue occurrence");
        assert_eq!(first.scheduled_at_unix_seconds(), 100);
        book.commit(first, 1_000);
        assert_eq!(book.next_deadline(), Some(1_050));
        let second = book.take_due(1_050).expect("next occurrence");
        assert_eq!(second.run_number(), 2);
    }

    #[test]
    fn cancellation_prevents_future_occurrences() {
        let mut book = ScheduleBook::new(4);
        book.add(ScheduleSpec::new(id("cancel-me"), 100, 50, 3))
            .expect("add schedule");
        let cancelled = book.cancel(&id("cancel-me")).expect("cancel schedule");
        assert_eq!(cancelled.completed_runs(), 0);
        assert!(book.take_due(u64::MAX).is_none());
    }

    #[test]
    fn duplicate_ids_and_capacity_are_rejected() {
        let mut book = ScheduleBook::new(1);
        let spec = ScheduleSpec::new(id("only"), 100, 0, 1);
        book.add(spec).expect("first add");
        assert!(book.add(spec).is_err());
        assert!(
            book.add(ScheduleSpec::new(id("overflow"), 100, 0, 1))
                .is_err()
        );
    }

    #[test]
    fn invalid_repeat_shapes_are_rejected() {
        assert!(ScheduleConfig::new(0, 0).is_err());
        assert!(ScheduleConfig::new(0, 2).is_err());
        assert!(ScheduleConfig::new(10, 1).is_ok());
        assert!(ScheduleConfig::new(10, 0).is_err());
    }
}
