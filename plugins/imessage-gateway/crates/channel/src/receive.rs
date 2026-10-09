//! Generic receive-loop scaffold owned by a channel Plugin's task.
//!
//! A channel Plugin builds a [`ReceiveControl`] and a [`ReceiveRuntime`] in
//! `register`, keeps the control for its endpoints, and races the runtime
//! against its `PluginTaskToken` in one Embassy task spawned in `start`. While
//! the channel is not enabled for receiving, the runtime parks on a signal and
//! holds no receive slot. Once enabled it holds one [`ReceiveSlotSource`]
//! lease and runs the channel's [`ReceiveChannel::receive`] session, restarting
//! it immediately while healthy and backing off only after errors.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;

use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};

/// Longest wait a server may request through [`ReceiveError::retry_after`].
const MAX_RETRY_AFTER: Duration = Duration::from_secs(600);

/// A source of receive-connection leases.
///
/// The System's receive slot pool implements this through a small adapter in
/// the channel Plugin; a channel that receives over the device's own HTTP
/// server and holds no outbound connection uses [`UnlimitedSlots`].
pub trait ReceiveSlotSource: 'static {
    /// One held slot; dropping it frees the slot.
    type Lease: 'static;

    /// Takes a free slot, or returns `None` when every slot is in use.
    fn acquire(&self) -> Option<Self::Lease>;

    /// Number of slots, shown to the user when none is free.
    fn capacity(&self) -> usize;
}

/// Slot source for receive paths that hold no outbound connection, such as
/// a webhook served by the device; acquiring always succeeds.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnlimitedSlots;

impl ReceiveSlotSource for UnlimitedSlots {
    type Lease = ();

    fn acquire(&self) -> Option<()> {
        Some(())
    }

    fn capacity(&self) -> usize {
        usize::MAX
    }
}

/// Where a channel's receive loop is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveState {
    /// Not enabled for receiving, or the task stopped.
    Idle,
    /// Holding a slot and opening a session.
    Starting,
    /// The session reported a healthy connection or poll.
    Receiving,
    /// Enabled, but every receive slot is in use; retried as slots free.
    NoSlot,
    /// The last session failed with this message; retrying after backoff, or
    /// waiting for a restart when the error halted the loop.
    Error(String),
}

impl ReceiveState {
    /// JSON name: `idle`, `starting`, `receiving`, `no_slot`, or `error`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Receiving => "receiving",
            Self::NoSlot => "no_slot",
            Self::Error(_) => "error",
        }
    }

    /// The error message, for [`Self::Error`].
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Error(message) => Some(message),
            _ => None,
        }
    }
}

/// Every receive slot is in use. The channel stays enabled and takes a slot
/// as soon as one frees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("every receive slot is in use")]
pub struct NoSlot;

/// Time limits of the receive runtime.
#[derive(Clone, Copy, Debug)]
pub struct ReceiveTiming {
    /// First reconnect delay after an error.
    pub initial_backoff: Duration,
    /// Reconnect delay cap; each consecutive error doubles the delay.
    pub max_backoff: Duration,
    /// Interval between attempts to take a slot while none is free.
    pub slot_retry: Duration,
}

impl ReceiveTiming {
    /// Limits used on the device: 1 s doubling to 30 s, slot retry every 5 s.
    pub const DEVICE: Self = Self {
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        slot_retry: Duration::from_secs(5),
    };
}

/// Why a receive session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveError {
    message: String,
    retry_after: Option<Duration>,
    halt: bool,
}

impl ReceiveError {
    /// A failure retried after the reconnect backoff. `message` is shown in
    /// the portal.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
            halt: false,
        }
    }

    /// A failure that retrying cannot fix, such as an expired login: the
    /// loop keeps its slot, reports `error`, and waits for
    /// [`ReceiveControl::restart`] or a mode change.
    #[must_use]
    pub fn halt(message: impl Into<String>) -> Self {
        Self {
            halt: true,
            ..Self::new(message)
        }
    }

    /// Waits at least `delay` (capped at ten minutes) before reconnecting,
    /// as a server's `retry_after` or `Retry-After` asks.
    #[must_use]
    pub fn retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay.min(MAX_RETRY_AFTER));
        self
    }

    /// The message shown in the portal.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Future of one receive session.
pub type ReceiveFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ReceiveError>> + 'a>>;

/// A channel's receive implementation.
pub trait ReceiveChannel<Lease>: 'static {
    /// Runs one receive session over `lease` until it fails or ends.
    ///
    /// Call [`ReceiveSession::receiving`] after each healthy poll or once a
    /// push connection is established; that marks the channel `receiving` and
    /// resets the backoff. Await `IMessageGateway::ready` before fetching each
    /// batch, check each sender with `Owners::classify`, and publish owner
    /// messages. Return `Err` on failure. Return `Ok(())` when the server
    /// ended a healthy session; the runtime reconnects at once.
    ///
    /// The runtime drops the future when the channel leaves `send_receive`,
    /// on [`ReceiveControl::restart`], and on Plugin cancellation, so every
    /// await must tolerate cancellation; a dropped connection is reopened by
    /// the next session.
    fn receive<'a>(
        &'a self,
        lease: &'a mut Lease,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a>;
}

/// Type-erased receive runtime moved into the channel Plugin's task.
pub type ReceiveRuntime = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// State shared by the control, the runtime, and running sessions.
struct ReceiveShared {
    state: RefCell<ReceiveState>,
    enabled: Cell<bool>,
    /// Bumped by [`ReceiveControl::restart`].
    generation: Cell<u32>,
    /// Whether the runtime holds a lease.
    leased: Cell<bool>,
    /// Whether the current session reported a healthy connection.
    healthy: Cell<bool>,
    /// Set once the runtime is dropped.
    stopped: Cell<bool>,
    changed: Signal<NoopRawMutex, ()>,
}

impl ReceiveShared {
    fn set(&self, state: ReceiveState) {
        self.state.replace(state);
    }
}

/// Handle a running session uses to report progress.
#[derive(Clone, Copy)]
pub struct ReceiveSession<'a>(&'a ReceiveShared);

impl ReceiveSession<'_> {
    /// Marks the channel `receiving` and resets the reconnect backoff.
    pub fn receiving(&self) {
        self.0.healthy.set(true);
        if *self.0.state.borrow() != ReceiveState::Receiving {
            self.0.set(ReceiveState::Receiving);
        }
    }
}

/// Endpoint-side control of a channel's receive loop.
///
/// Shared by the channel's endpoints and its [`ReceiveRuntime`].
pub struct ReceiveControl<Slots: ReceiveSlotSource> {
    slots: Slots,
    /// Lease taken by [`Self::set_enabled`] for the runtime to pick up.
    pending: RefCell<Option<Slots::Lease>>,
    shared: ReceiveShared,
}

impl<Slots: ReceiveSlotSource> ReceiveControl<Slots> {
    /// Creates a disabled control over `slots`.
    #[must_use]
    pub fn new(slots: Slots) -> Self {
        Self {
            slots,
            pending: RefCell::new(None),
            shared: ReceiveShared {
                state: RefCell::new(ReceiveState::Idle),
                enabled: Cell::new(false),
                generation: Cell::new(0),
                leased: Cell::new(false),
                healthy: Cell::new(false),
                stopped: Cell::new(false),
                changed: Signal::new(),
            },
        }
    }

    /// Enables or disables receiving.
    ///
    /// Pass `configured && mode.receives()`. Enabling takes a slot at once
    /// when the loop holds none, so the caller can report a full pool;
    /// disabling makes the runtime drop its session and slot.
    ///
    /// # Errors
    ///
    /// Returns [`NoSlot`] when enabling finds every slot in use. The control
    /// stays enabled, reports `no_slot`, and the runtime keeps retrying.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), NoSlot> {
        let changed = self.shared.enabled.replace(enabled) != enabled;
        if !enabled {
            self.pending.take();
            if changed {
                self.shared.set(ReceiveState::Idle);
                self.shared.changed.signal(());
            }
            return Ok(());
        }
        if self.shared.stopped.get() || self.shared.leased.get() || self.pending.borrow().is_some()
        {
            return Ok(());
        }
        let Some(lease) = self.slots.acquire() else {
            self.shared.set(ReceiveState::NoSlot);
            self.shared.changed.signal(());
            return Err(NoSlot);
        };
        self.pending.replace(Some(lease));
        self.shared.set(ReceiveState::Starting);
        self.shared.changed.signal(());
        Ok(())
    }

    /// Ends the running session and starts a new one at once, keeping the
    /// slot; also resumes a loop halted by [`ReceiveError::halt`]. Call it
    /// after the channel's configuration changes.
    pub fn restart(&self) {
        let generation = self.shared.generation.get().wrapping_add(1);
        self.shared.generation.set(generation);
        self.shared.changed.signal(());
    }

    /// Whether receiving is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.shared.enabled.get()
    }

    /// Current receive state.
    #[must_use]
    pub fn state(&self) -> ReceiveState {
        self.shared.state.borrow().clone()
    }

    /// Number of receive slots, shown with `no_slot`.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    fn take_lease(&self) -> Option<Slots::Lease> {
        self.pending.take().or_else(|| self.slots.acquire())
    }
}

/// Builds the runtime future that must run as the channel Plugin's receive
/// task, raced against its `PluginTaskToken`.
///
/// Dropping it, as cancellation does, drops the running session and the
/// lease and leaves the control reporting `idle`.
pub fn receive_runtime<Slots, Channel>(
    control: Rc<ReceiveControl<Slots>>,
    channel: Rc<Channel>,
    timing: ReceiveTiming,
) -> ReceiveRuntime
where
    Slots: ReceiveSlotSource,
    Channel: ReceiveChannel<Slots::Lease>,
{
    Box::pin(
        Runner {
            control,
            channel,
            timing,
        }
        .run(),
    )
}

/// Marks the control stopped and releases a pending lease when the runtime
/// is dropped.
struct StopOnDrop<Slots: ReceiveSlotSource>(Rc<ReceiveControl<Slots>>);

impl<Slots: ReceiveSlotSource> Drop for StopOnDrop<Slots> {
    fn drop(&mut self) {
        let control = &self.0;
        control.shared.stopped.set(true);
        control.shared.leased.set(false);
        control.pending.take();
        control.shared.set(ReceiveState::Idle);
    }
}

/// Clears the leased flag when the runtime releases its lease.
struct Leased<'a>(&'a ReceiveShared);

impl Drop for Leased<'_> {
    fn drop(&mut self) {
        self.0.leased.set(false);
    }
}

struct Runner<Slots: ReceiveSlotSource, Channel> {
    control: Rc<ReceiveControl<Slots>>,
    channel: Rc<Channel>,
    timing: ReceiveTiming,
}

impl<Slots, Channel> Runner<Slots, Channel>
where
    Slots: ReceiveSlotSource,
    Channel: ReceiveChannel<Slots::Lease>,
{
    async fn run(self) {
        let _stopped = StopOnDrop(Rc::clone(&self.control));
        let shared = &self.control.shared;
        loop {
            if !shared.enabled.get() {
                shared.set(ReceiveState::Idle);
                shared.changed.wait().await;
                continue;
            }
            let Some(mut lease) = self.control.take_lease() else {
                shared.set(ReceiveState::NoSlot);
                let _woken =
                    select(shared.changed.wait(), Timer::after(self.timing.slot_retry)).await;
                continue;
            };
            shared.leased.set(true);
            let leased = Leased(shared);
            self.hold(&mut lease).await;
            drop(lease);
            drop(leased);
        }
    }

    /// Runs sessions over one lease until receiving is disabled.
    async fn hold(&self, lease: &mut Slots::Lease) {
        let shared = &self.control.shared;
        let mut backoff = self.timing.initial_backoff;
        while shared.enabled.get() {
            let generation = shared.generation.get();
            shared.healthy.set(false);
            shared.set(ReceiveState::Starting);
            let Some(outcome) = self.session(lease, generation).await else {
                // Disabled or restarted: reconnect without delay.
                backoff = self.timing.initial_backoff;
                continue;
            };
            if shared.healthy.get() {
                backoff = self.timing.initial_backoff;
            }
            let delay = match outcome {
                Ok(()) if shared.healthy.get() => continue,
                // Ended before ever being healthy: pace it like an error.
                Ok(()) => backoff,
                Err(error) if error.halt => {
                    log::warn!("receive halted: {}", error.message);
                    shared.set(ReceiveState::Error(error.message));
                    self.wait_for_change(generation).await;
                    backoff = self.timing.initial_backoff;
                    continue;
                }
                Err(error) => {
                    log::warn!("receive failed: {}", error.message);
                    shared.set(ReceiveState::Error(error.message));
                    error
                        .retry_after
                        .map_or(backoff, |after| after.max(backoff))
                }
            };
            if self.pause(delay, generation).await {
                backoff = self.timing.initial_backoff;
                continue;
            }
            backoff = backoff
                .checked_mul(2)
                .unwrap_or(self.timing.max_backoff)
                .min(self.timing.max_backoff);
        }
    }

    /// Runs one session, returning `None` when it was ended by a disable or
    /// a restart.
    async fn session(
        &self,
        lease: &mut Slots::Lease,
        generation: u32,
    ) -> Option<Result<(), ReceiveError>> {
        let shared = &self.control.shared;
        let mut session = self
            .channel
            .receive(lease, ReceiveSession(&self.control.shared));
        loop {
            match select(shared.changed.wait(), session.as_mut()).await {
                Either::First(()) => {
                    if !shared.enabled.get() || shared.generation.get() != generation {
                        return None;
                    }
                }
                Either::Second(outcome) => return Some(outcome),
            }
        }
    }

    /// Waits `delay`, returning `true` early when receiving is disabled or
    /// restarted meanwhile.
    async fn pause(&self, delay: Duration, generation: u32) -> bool {
        let shared = &self.control.shared;
        let deadline = Instant::now().saturating_add(delay);
        loop {
            match select(shared.changed.wait(), Timer::at(deadline)).await {
                Either::First(()) => {
                    if !shared.enabled.get() || shared.generation.get() != generation {
                        return true;
                    }
                }
                Either::Second(()) => return false,
            }
        }
    }

    /// Waits until receiving is disabled or restarted.
    async fn wait_for_change(&self, generation: u32) {
        let shared = &self.control.shared;
        while shared.enabled.get() && shared.generation.get() == generation {
            shared.changed.wait().await;
        }
    }
}
