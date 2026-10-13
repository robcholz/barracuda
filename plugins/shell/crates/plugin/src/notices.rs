//! The `Notifications` capability: what the notification center lists.

use alloc::{string::String, vec::Vec};
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::Serialize;

/// Notices kept before the oldest is dropped.
pub const MAX_NOTICES: usize = 8;
/// The longest title or body kept, in characters; longer text is cut.
pub const MAX_NOTICE_CHARS: usize = 64;

/// One notice in the notification center.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Notice {
    /// Stable for the notice's life.
    pub id: u32,
    /// What happened, in a few words.
    pub title: String,
    /// Who or what it is from, or more detail.
    pub body: String,
}

/// The notification center's notices; published by the `shell` Plugin.
///
/// Any Plugin may post a notice for the person; the system UI lists them,
/// newest first, marks the home screen while one is unread, and lets the
/// person dismiss them.
pub struct Notifications {
    notices: RefCell<Vec<Notice>>,
    next_id: Cell<u32>,
    unread: Cell<bool>,
    changed: Signal<NoopRawMutex, ()>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifications {
    /// An empty notification center.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            notices: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            unread: Cell::new(false),
            changed: Signal::new(),
        }
    }

    /// Posts a notice and returns its ID. Text longer than
    /// [`MAX_NOTICE_CHARS`] is cut; beyond [`MAX_NOTICES`], the oldest
    /// notice is dropped.
    pub fn post(&self, title: &str, body: &str) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        let mut notices = self.notices.borrow_mut();
        if notices.len() == MAX_NOTICES {
            notices.remove(0);
        }
        notices.push(Notice {
            id,
            title: cut(title),
            body: cut(body),
        });
        drop(notices);
        self.unread.set(true);
        self.changed.signal(());
        id
    }

    /// Removes notice `id`; returns whether it was there.
    pub fn dismiss(&self, id: u32) -> bool {
        let mut notices = self.notices.borrow_mut();
        let before = notices.len();
        notices.retain(|notice| notice.id != id);
        let removed = notices.len() != before;
        drop(notices);
        if removed {
            self.changed.signal(());
        }
        removed
    }

    /// Removes every notice.
    pub fn clear(&self) {
        self.notices.borrow_mut().clear();
        self.unread.set(false);
        self.changed.signal(());
    }

    /// Every notice, newest first.
    #[must_use]
    pub fn list(&self) -> Vec<Notice> {
        self.notices.borrow().iter().rev().cloned().collect()
    }

    /// Whether a notice arrived since the person last opened the center.
    #[must_use]
    pub fn unread(&self) -> bool {
        self.unread.get()
    }

    /// Marks every notice read.
    pub fn mark_read(&self) {
        self.unread.set(false);
    }

    /// Waits until a notice is posted or removed. One waiter, the system
    /// UI, is served.
    pub async fn changed(&self) {
        self.changed.wait().await;
    }
}

fn cut(text: &str) -> String {
    text.chars().take(MAX_NOTICE_CHARS).collect()
}
