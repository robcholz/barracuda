use alloc::{boxed::Box, rc::Rc};
use core::cell::RefCell;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventEmitter, RegisterContext,
    RpcClient, RunContext, UnregisterContext,
};
use barracuda_time_component::now::{Now, TimeNowRequest};
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use getset::CopyGetters;

use crate::{
    cancel::{Cancel, cancel_handler},
    event::{SchedulerTriggered, Triggered},
    model::unix_seconds,
    schedule::{Schedule, schedule_handler},
    state::ScheduleBook,
};

/// Scheduler capacity and maximum RTC recheck interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct SchedulerConfig {
    /// Maximum number of simultaneously live schedules.
    #[getset(get_copy = "pub")]
    capacity: usize,
    /// Longest period scheduler may sleep without re-reading `time.now`.
    #[getset(get_copy = "pub")]
    max_recheck_millis: u64,
}

impl SchedulerConfig {
    /// Creates an explicit scheduler policy.
    #[must_use]
    pub const fn new(capacity: usize, max_recheck_millis: u64) -> Self {
        Self {
            capacity,
            max_recheck_millis,
        }
    }
}

pub(crate) struct SchedulerShared {
    pub(crate) book: RefCell<ScheduleBook>,
    pub(crate) changed: Signal<NoopRawMutex, ()>,
}

/// Cloneable control passed to reusable scheduler RPC handlers.
#[derive(Clone)]
pub struct SchedulerControl {
    pub(crate) shared: Rc<SchedulerShared>,
}

impl SchedulerShared {
    fn new(capacity: usize) -> Self {
        Self {
            book: RefCell::new(ScheduleBook::new(capacity)),
            changed: Signal::new(),
        }
    }
}

/// Event Router Component that schedules against typed `time.now` readings.
pub struct SchedulerComponent {
    config: SchedulerConfig,
    control: SchedulerControl,
}

impl SchedulerComponent {
    /// Creates an empty in-memory scheduler.
    #[must_use]
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            control: SchedulerControl {
                shared: Rc::new(SchedulerShared::new(config.capacity)),
            },
        }
    }
}

impl<const M: usize> Component<M> for SchedulerComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<Schedule, _>(schedule_handler(self.control.clone()))?;
        context.register_rpc::<Cancel, _>(cancel_handler(self.control.clone()))
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            let emitter = EventEmitter::<M>::new(client.clone());
            loop {
                let Some(now_unix_seconds) = read_time(&client).await? else {
                    wait_for_change(
                        &self.control.shared,
                        Duration::from_millis(self.config.max_recheck_millis.max(1)),
                    )
                    .await;
                    continue;
                };

                loop {
                    let occurrence = self.control.shared.book.borrow().take_due(now_unix_seconds);
                    let Some(occurrence) = occurrence else {
                        break;
                    };
                    emitter
                        .emit::<SchedulerTriggered>(Triggered::new(occurrence))
                        .await
                        .map_err(ComponentError::lifecycle)?;
                    self.control
                        .shared
                        .book
                        .borrow_mut()
                        .commit(occurrence, now_unix_seconds);
                }

                let deadline = self.control.shared.book.borrow().next_deadline();
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

async fn read_time(client: &RpcClient) -> ComponentResult<Option<u64>> {
    let result = client.call::<Now>(TimeNowRequest::new())?.await?;
    match result {
        Ok(frame) => Ok(unix_seconds(*frame.view()?).ok()),
        Err(_unavailable) => Ok(None),
    }
}

async fn wait_for_change(shared: &SchedulerShared, duration: Duration) {
    match select(Timer::after(duration), shared.changed.wait()).await {
        Either::First(()) | Either::Second(()) => {}
    }
}
