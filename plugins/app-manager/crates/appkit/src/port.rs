//! What a running app shares with its Lua program.
//!
//! A [`Port`] is `Send + Sync`: the manager changes the app's state and queues
//! lifecycle events; the program waits on one merged stream of lifecycle
//! events, view calls, and timer ticks.

use crate::storage::Storage;
use alloc::{collections::BTreeMap, collections::VecDeque, string::String};
use barracuda_uikit::{ViewCall, ViewChannel, ViewNext};
use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic_util::Arc;
use serde::Serialize;

/// The shortest interval `app.events{ every = ms }` accepts.
pub const MIN_TICK_MS: u64 = 100;

/// Where a running app is in its lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppState {
    /// In the foreground and taking input.
    Active,
    /// In the foreground but not taking input: an overlay covers it or a
    /// transition is running.
    Inactive,
    /// Left the foreground; saving state within the grace period.
    Background,
    /// Parked: its program waits for events and its view tree is released.
    Suspended,
    /// A headless app running as a service.
    Running,
    /// Asked to exit; ends within the grace period or is cancelled.
    Terminating,
}

/// A lifecycle event, as Lua sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lifecycle {
    Active,
    Inactive,
    Background,
    Foreground,
    Terminate,
    Back,
}

impl Lifecycle {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Lifecycle::Active => "app:active",
            Lifecycle::Inactive => "app:inactive",
            Lifecycle::Background => "app:background",
            Lifecycle::Foreground => "app:foreground",
            Lifecycle::Terminate => "app:terminate",
            Lifecycle::Back => "app:back",
        }
    }
}

/// One step of the app's event loop.
pub(crate) enum AppEvent {
    Lifecycle(Lifecycle),
    Tick,
    View(ViewCall),
}

struct PortState {
    state: AppState,
    events: VecDeque<Lifecycle>,
    waker: Option<Waker>,
    /// Event waits in flight; non-zero means the program is parked.
    waiting: usize,
    ended: bool,
    every: Option<Duration>,
    next_tick: Option<Instant>,
    /// Pages the app has open above its first; a back gesture pops one.
    depth: u32,
}

pub(crate) struct Port {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) view: Option<ViewChannel>,
    pub(crate) storage: Storage,
    state: critical_section::Mutex<RefCell<PortState>>,
}

impl Port {
    pub(crate) fn new(
        id: String,
        name: String,
        version: String,
        view: Option<ViewChannel>,
        storage: Storage,
        state: AppState,
    ) -> Self {
        Self {
            id,
            name,
            version,
            view,
            storage,
            state: critical_section::Mutex::new(RefCell::new(PortState {
                state,
                events: VecDeque::new(),
                waker: None,
                waiting: 0,
                ended: false,
                every: None,
                next_tick: None,
                depth: 0,
            })),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut PortState) -> R) -> R {
        critical_section::with(|cs| f(&mut self.state.borrow(cs).borrow_mut()))
    }

    /// Moves the app to `state` and queues `events` for its program, in order.
    pub(crate) fn transition(&self, state: AppState, events: &[Lifecycle]) {
        self.with(|port| {
            port.state = state;
            port.events.extend(events.iter().copied());
            if let Some(waker) = port.waker.take() {
                waker.wake();
            }
        });
    }

    /// Whether the program is parked in `app.events()`.
    pub(crate) fn is_waiting(&self) -> bool {
        self.with(|port| port.waiting > 0)
    }

    /// Ends the event loop: queued lifecycle events drain, then it stops.
    pub(crate) fn end(&self) {
        self.with(|port| {
            port.ended = true;
            if let Some(waker) = port.waker.take() {
                waker.wake();
            }
        });
        if let Some(view) = &self.view {
            view.close();
        }
    }

    /// Records how many pages the app has open above its first.
    pub(crate) fn set_depth(&self, depth: u32) {
        self.with(|port| port.depth = depth);
    }

    /// Pages the app has open above its first.
    pub(crate) fn depth(&self) -> u32 {
        self.with(|port| port.depth)
    }

    /// Delivers `app:tick` every `every`, starting one interval from now.
    pub(crate) fn set_every(&self, every: Option<Duration>) {
        self.with(|port| {
            port.every = every;
            port.next_tick = every.map(|every| Instant::now() + every);
            if let Some(waker) = port.waker.take() {
                waker.wake();
            }
        });
    }

    /// Waits for the next event; `None` once the loop has ended.
    pub(crate) fn next(port: &Arc<Self>) -> NextAppEvent {
        port.with(|port| port.waiting += 1);
        NextAppEvent {
            port: Arc::clone(port),
            view: None,
            timer: None,
        }
    }
}

pub(crate) struct NextAppEvent {
    port: Arc<Port>,
    view: Option<ViewNext>,
    timer: Option<(Instant, Timer)>,
}

impl Future for NextAppEvent {
    type Output = Option<AppEvent>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let snapshot = this.port.with(|port| {
            if let Some(event) = port.events.pop_front() {
                return Err(Some(AppEvent::Lifecycle(event)));
            }
            if port.ended {
                return Err(None);
            }
            port.waker = Some(cx.waker().clone());
            Ok((port.state, port.next_tick))
        });
        let (state, next_tick) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(event) => return Poll::Ready(event),
        };
        // Only an active app takes input.
        if state == AppState::Active
            && let Some(channel) = &this.port.view
        {
            let view = this.view.get_or_insert_with(|| channel.next());
            if let Poll::Ready(call) = Pin::new(view).poll(cx) {
                this.view = None;
                // A closed view stops delivering calls; lifecycle events go on.
                if let Some(call) = call {
                    return Poll::Ready(Some(AppEvent::View(call)));
                }
            }
        }
        // Ticks stop while the app leaves, parks, or exits.
        let ticking = matches!(
            state,
            AppState::Active | AppState::Inactive | AppState::Running
        );
        if let (true, Some(deadline)) = (ticking, next_tick) {
            let timer = match &mut this.timer {
                Some((at, timer)) if *at == deadline => timer,
                slot => &mut slot.insert((deadline, Timer::at(deadline))).1,
            };
            if Pin::new(timer).poll(cx).is_ready() {
                this.timer = None;
                this.port.with(|port| {
                    port.next_tick = port.every.map(|every| Instant::now() + every);
                });
                return Poll::Ready(Some(AppEvent::Tick));
            }
        }
        Poll::Pending
    }
}

impl Drop for NextAppEvent {
    fn drop(&mut self) {
        self.port
            .with(|port| port.waiting = port.waiting.saturating_sub(1));
    }
}

/// A port and whether its ticket has attached.
type Ticketed = (Arc<Port>, bool);

/// Tickets bind a program to its app; each attaches once.
pub(crate) struct Ports {
    entries: critical_section::Mutex<RefCell<BTreeMap<String, Ticketed>>>,
}

impl Ports {
    pub(crate) const fn new() -> Self {
        Self {
            entries: critical_section::Mutex::new(RefCell::new(BTreeMap::new())),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut BTreeMap<String, Ticketed>) -> R) -> R {
        critical_section::with(|cs| f(&mut self.entries.borrow(cs).borrow_mut()))
    }

    pub(crate) fn insert(&self, ticket: String, port: Arc<Port>) {
        self.with(|entries| entries.insert(ticket, (port, false)));
    }

    pub(crate) fn attach(&self, ticket: &str) -> Option<Arc<Port>> {
        self.with(|entries| {
            let (port, attached) = entries.get_mut(ticket)?;
            if *attached {
                return None;
            }
            *attached = true;
            Some(Arc::clone(port))
        })
    }

    pub(crate) fn remove(&self, ticket: &str) {
        self.with(|entries| entries.remove(ticket));
    }

    /// Ends every app's event loop; used when the package is revoked.
    pub(crate) fn end_all(&self) {
        let ports: alloc::vec::Vec<Arc<Port>> =
            self.with(|entries| entries.values().map(|(port, _)| Arc::clone(port)).collect());
        for port in ports {
            port.end();
        }
    }
}
