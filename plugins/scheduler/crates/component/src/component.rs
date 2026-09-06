use alloc::{boxed::Box, rc::Rc};

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventEmitter, RegisterContext,
    RunContext, Topic, UnregisterContext,
};
use barracuda_plugin::manager::{
    PluginEntryIterator as _, PluginReadTransaction as _, PluginStorage, StorageError,
};
use barracuda_time_plugin::UtcClock;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Timer};
use getset::CopyGetters;

use crate::{
    cancel::{Cancel, cancel_handler},
    event::SchedulerTriggered,
    json::Triggered,
    model::ScheduleId,
    schedule::{Schedule, schedule_handler},
    state::{PersistedSchedule, RecordError, ScheduleBook},
};

/// Scheduler timing policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct SchedulerConfig {
    /// Longest period scheduler may sleep without re-reading the UTC clock.
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

pub(crate) struct SchedulerShared<Storage> {
    pub(crate) book: Mutex<NoopRawMutex, ScheduleBook>,
    pub(crate) changed: Signal<NoopRawMutex, ()>,
    pub(crate) storage: Storage,
}

/// Cloneable control passed to reusable scheduler RPC handlers.
#[derive(Clone)]
pub(crate) struct SchedulerControl<Storage> {
    pub(crate) shared: Rc<SchedulerShared<Storage>>,
}

impl<Storage> SchedulerShared<Storage>
where
    Storage: PluginStorage,
{
    fn new(book: ScheduleBook, storage: Storage) -> Self {
        Self {
            book: Mutex::new(book),
            changed: Signal::new(),
            storage,
        }
    }

    pub(crate) async fn write(
        &self,
        id: ScheduleId,
        value: &PersistedSchedule,
    ) -> Result<(), SchedulerStorageError> {
        self.storage.put(id.as_str(), value).await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, id: ScheduleId) -> Result<(), SchedulerStorageError> {
        self.storage.delete(id.as_str()).await?;
        Ok(())
    }
}

/// Failure loading or saving Scheduler's persistent state.
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

/// Event Router Component that schedules against the typed UTC clock capability.
pub struct SchedulerComponent<Storage> {
    config: SchedulerConfig,
    control: SchedulerControl<Storage>,
    clock: Rc<UtcClock>,
}

impl<Storage> SchedulerComponent<Storage>
where
    Storage: PluginStorage,
{
    /// Restores Scheduler state from its Plugin-scoped key-value storage.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read or a record is invalid.
    pub async fn load(
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
            control: SchedulerControl {
                shared: Rc::new(SchedulerShared::new(book, storage)),
            },
            clock,
        })
    }
}

impl<Storage, const M: usize> Component<M> for SchedulerComponent<Storage>
where
    Storage: PluginStorage,
{
    fn name(&self) -> &'static str {
        "scheduler"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_json::<Schedule, _>(
            "*",
            schedule_handler(self.control.clone(), Rc::clone(&self.clock)),
        )?;
        context.register_json::<Cancel, _>("*", cancel_handler(self.control.clone()))
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            loop {
                let Some(now_unix_seconds) = read_time(&self.clock) else {
                    wait_for_change(
                        &self.control.shared,
                        Duration::from_millis(self.config.max_recheck_millis.max(1)),
                    )
                    .await;
                    continue;
                };

                loop {
                    let occurrence = self
                        .control
                        .shared
                        .book
                        .lock()
                        .await
                        .take_due(now_unix_seconds);
                    let Some(occurrence) = occurrence else {
                        break;
                    };
                    let topic = Topic::try_from(occurrence.id().as_str())
                        .map_err(ComponentError::lifecycle)?;
                    emitter
                        .emit_to::<SchedulerTriggered>(&topic, &Triggered::new(occurrence))
                        .await
                        .map_err(ComponentError::lifecycle)?;
                    let mut book = self.control.shared.book.lock().await;
                    if book.commit(occurrence, now_unix_seconds) {
                        match book.persisted(&occurrence.id()) {
                            Some(record) => {
                                self.control.shared.write(occurrence.id(), &record).await
                            }
                            None => self.control.shared.delete(occurrence.id()).await,
                        }
                        .map_err(ComponentError::lifecycle)?;
                    }
                }

                let deadline = self.control.shared.book.lock().await.next_deadline();
                let Some(deadline) = deadline else {
                    self.control.shared.changed.wait().await;
                    continue;
                };
                let delay = deadline
                    .saturating_sub(now_unix_seconds)
                    .saturating_mul(1_000)
                    .min(self.config.max_recheck_millis.max(1))
                    .max(1);
                wait_for_change(&self.control.shared, Duration::from_millis(delay)).await;
            }
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn read_time(clock: &UtcClock) -> Option<u64> {
    clock.now().ok().map(|now| u64::from(now) / 1_000)
}

async fn wait_for_change<Storage>(shared: &SchedulerShared<Storage>, duration: Duration) {
    match select(Timer::after(duration), shared.changed.wait()).await {
        Either::First(()) | Either::Second(()) => {}
    }
}
