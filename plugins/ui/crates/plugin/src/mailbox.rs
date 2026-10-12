//! The only state shared between view owners and the UI task.
//!
//! Owners write values and wait for events here; the UI task applies the writes
//! to the view and posts events. Everything inside is `Send`, guarded by a
//! critical section, so view channels stay `Send + Sync` for owners on any task.

use crate::datum::{ViewType, ViewValue};
use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    vec::Vec,
};
use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use portable_atomic_util::Arc;

/// Events a view may queue for its owner before older ones are dropped.
pub const MAX_EVENTS: usize = 16;

/// The view contract owners write against.
#[derive(Debug)]
pub(crate) struct Schema {
    pub(crate) slots: Vec<(String, ViewType)>,
}

/// A handler call waiting for the owner.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Event {
    pub(crate) handler: String,
    pub(crate) args: Vec<ViewValue>,
}

struct Entry {
    closed: bool,
    writes: Vec<Option<ViewValue>>,
    events: VecDeque<Event>,
    waker: Option<Waker>,
}

/// Why a mailbox operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MailboxError {
    /// The view was closed by its app or the UI.
    Closed,
    /// The slot index is outside the view's contract.
    UnknownSlot,
}

pub(crate) struct Mailbox {
    entries: critical_section::Mutex<RefCell<BTreeMap<u32, Entry>>>,
    wake: Signal<CriticalSectionRawMutex, ()>,
}

impl Mailbox {
    pub(crate) fn new() -> Self {
        Self {
            entries: critical_section::Mutex::new(RefCell::new(BTreeMap::new())),
            wake: Signal::new(),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut BTreeMap<u32, Entry>) -> R) -> R {
        critical_section::with(|cs| f(&mut self.entries.borrow(cs).borrow_mut()))
    }

    /// Wakes the UI task to apply writes.
    pub(crate) async fn wait(&self) {
        self.wake.wait().await;
    }

    pub(crate) fn insert(&self, id: u32, slots: usize) {
        let writes = (0..slots).map(|_| None).collect();
        self.with(|entries| {
            entries.insert(
                id,
                Entry {
                    closed: false,
                    writes,
                    events: VecDeque::new(),
                    waker: None,
                },
            );
        });
    }

    pub(crate) fn write(&self, id: u32, slot: usize, datum: ViewValue) -> Result<(), MailboxError> {
        self.with(|entries| {
            let entry = entries
                .get_mut(&id)
                .filter(|entry| !entry.closed)
                .ok_or(MailboxError::Closed)?;
            *entry
                .writes
                .get_mut(slot)
                .ok_or(MailboxError::UnknownSlot)? = Some(datum);
            Ok(())
        })?;
        self.wake.signal(());
        Ok(())
    }

    /// Takes the latest write to every slot of view `id`.
    pub(crate) fn take_writes(&self, id: u32) -> Vec<(usize, ViewValue)> {
        self.with(|entries| {
            entries
                .get_mut(&id)
                .map(|entry| {
                    entry
                        .writes
                        .iter_mut()
                        .enumerate()
                        .filter_map(|(slot, write)| write.take().map(|datum| (slot, datum)))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// Queues an event for the owner; returns `false` when the view is closed or full.
    pub(crate) fn post(&self, id: u32, event: Event) -> bool {
        self.with(|entries| {
            let Some(entry) = entries.get_mut(&id).filter(|entry| !entry.closed) else {
                return false;
            };
            if entry.events.len() >= MAX_EVENTS {
                return false;
            }
            entry.events.push_back(event);
            if let Some(waker) = entry.waker.take() {
                waker.wake();
            }
            true
        })
    }

    /// Drops queued events, so nothing reaches the owner after its view stops taking input.
    pub(crate) fn clear_events(&self, id: u32) {
        self.with(|entries| {
            if let Some(entry) = entries.get_mut(&id) {
                entry.events.clear();
            }
        });
    }

    fn poll_event(&self, id: u32, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        self.with(|entries| {
            let Some(entry) = entries.get_mut(&id) else {
                return Poll::Ready(None);
            };
            if let Some(event) = entry.events.pop_front() {
                return Poll::Ready(Some(event));
            }
            if entry.closed {
                return Poll::Ready(None);
            }
            entry.waker = Some(cx.waker().clone());
            Poll::Pending
        })
    }

    /// Waits for the next event of view `id`; `None` once it is closed and drained.
    pub(crate) fn next(mailbox: &Arc<Self>, id: u32) -> NextEvent {
        NextEvent {
            mailbox: Arc::clone(mailbox),
            id,
        }
    }

    pub(crate) fn is_closed(&self, id: u32) -> bool {
        self.with(|entries| entries.get(&id).is_none_or(|entry| entry.closed))
    }

    /// Ends view `id` for its owner: writes fail and waiting `next` calls return `None`.
    pub(crate) fn close(&self, id: u32) {
        self.with(|entries| {
            if let Some(entry) = entries.get_mut(&id) {
                entry.closed = true;
                if let Some(waker) = entry.waker.take() {
                    waker.wake();
                }
            }
        });
        self.wake.signal(());
    }

    pub(crate) fn remove(&self, id: u32) {
        self.close(id);
        self.with(|entries| entries.remove(&id));
    }

    /// Closes every view; used when the Plugin stops.
    pub(crate) fn close_all(&self) {
        let ids: Vec<u32> = self.with(|entries| entries.keys().copied().collect());
        for id in ids {
            self.close(id);
        }
    }
}

/// Waits for the next handler call of one view.
pub(crate) struct NextEvent {
    mailbox: Arc<Mailbox>,
    id: u32,
}

impl Future for NextEvent {
    type Output = Option<Event>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.mailbox.poll_event(self.id, cx)
    }
}
