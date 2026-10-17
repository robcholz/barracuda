//! The `Notifications` capability: what the notification center lists, and
//! the questions it asks the person.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use serde::Serialize;

/// Notices kept before the oldest is dropped.
pub const MAX_NOTICES: usize = 8;
/// The longest title or body kept, in characters; longer text is cut.
pub const MAX_NOTICE_CHARS: usize = 64;
/// The longest answer label kept, in characters; longer labels are cut.
pub const MAX_LABEL_CHARS: usize = 6;
/// What a question's buttons say unless its asker names them.
pub const DEFAULT_LABELS: (&str, &str) = ("允许", "拒绝");
/// How often an unanswered question is checked for an answer.
const ANSWER_POLL: Duration = Duration::from_millis(100);

/// One notice in the notification center.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Notice {
    /// Stable for the notice's life.
    pub id: u32,
    /// What happened, in a few words.
    pub title: String,
    /// Who or what it is from, or more detail.
    pub body: String,
    /// The system icon it is shown with, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Whether it asks the person to allow or deny something.
    pub question: bool,
    /// What a question's allow button says; empty for other notices.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub allow: String,
    /// What a question's deny button says; empty for other notices.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub deny: String,
}

/// The notification center's notices; published by the `app-manager` Plugin.
///
/// Any Plugin may post a notice for the person, or ask a question that the
/// person allows or denies on its card. The system UI lists questions first
/// and other notices newest first, marks the home screen while one is unread,
/// and lets the person dismiss them.
pub struct Notifications {
    notices: RefCell<Vec<Notice>>,
    /// Answers to questions, until their asker collects them; `None` when
    /// the question was dismissed unanswered.
    answers: RefCell<BTreeMap<u32, Option<bool>>>,
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
            answers: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(1),
            unread: Cell::new(false),
            changed: Signal::new(),
        }
    }

    fn add(
        &self,
        title: &str,
        body: &str,
        icon: Option<&str>,
        labels: Option<(&str, &str)>,
    ) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        let mut notices = self.notices.borrow_mut();
        if notices.len() == MAX_NOTICES {
            let oldest = notices.remove(0);
            self.close(oldest.id, None);
        }
        notices.push(Notice {
            id,
            title: cut(title),
            body: cut(body),
            icon: icon.map(String::from),
            question: labels.is_some(),
            allow: labels.map_or_else(String::new, |(allow, _)| label(allow, DEFAULT_LABELS.0)),
            deny: labels.map_or_else(String::new, |(_, deny)| label(deny, DEFAULT_LABELS.1)),
        });
        drop(notices);
        self.unread.set(true);
        self.changed.signal(());
        id
    }

    /// Posts a notice and returns its ID. Text longer than
    /// [`MAX_NOTICE_CHARS`] is cut; beyond [`MAX_NOTICES`], the oldest notice
    /// is dropped. `icon` names a system icon.
    pub fn post(&self, title: &str, body: &str, icon: Option<&str>) -> u32 {
        self.add(title, body, icon, None)
    }

    /// Asks the person to allow or deny what `title` describes, on a card
    /// listed before every other notice, and returns its ID for
    /// [`Notifications::answer`].
    pub fn ask(&self, title: &str, body: &str) -> u32 {
        self.ask_with(title, body, DEFAULT_LABELS)
    }

    /// Asks as [`Notifications::ask`] does, with buttons that say `labels`
    /// (allow, deny). A label longer than [`MAX_LABEL_CHARS`] is cut; an
    /// empty one says what the default does.
    pub fn ask_with(&self, title: &str, body: &str, labels: (&str, &str)) -> u32 {
        let id = self.add(title, body, None, Some(labels));
        self.answers.borrow_mut().insert(id, None);
        id
    }

    /// Records the person's answer to question `id` and removes its card;
    /// returns whether it was waiting for one.
    pub fn decide(&self, id: u32, allowed: bool) -> bool {
        let asked = self.answers.borrow().contains_key(&id);
        if asked {
            self.answers.borrow_mut().insert(id, Some(allowed));
            self.remove(id);
        }
        asked
    }

    /// Waits for the person's answer to question `id`: `Some(allowed)`, or
    /// `None` when its card was dismissed or dropped unanswered or no such
    /// question is open.
    pub async fn answer(&self, id: u32) -> Option<bool> {
        loop {
            match self.answers.borrow().get(&id) {
                None => return None,
                Some(Some(_)) => break,
                Some(None) if !self.notices.borrow().iter().any(|notice| notice.id == id) => break,
                Some(None) => {}
            }
            Timer::after(ANSWER_POLL).await;
        }
        self.answers.borrow_mut().remove(&id).flatten()
    }

    fn close(&self, id: u32, answer: Option<bool>) {
        if let Some(open) = self.answers.borrow_mut().get_mut(&id)
            && open.is_none()
        {
            *open = answer;
        }
    }

    fn remove(&self, id: u32) -> bool {
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

    /// Removes notice `id`, leaving a question unanswered; returns whether
    /// it was there.
    pub fn dismiss(&self, id: u32) -> bool {
        self.close(id, None);
        self.remove(id)
    }

    /// Removes every notice, leaving questions unanswered.
    pub fn clear(&self) {
        let ids: Vec<u32> = self
            .notices
            .borrow()
            .iter()
            .map(|notice| notice.id)
            .collect();
        for id in ids {
            self.close(id, None);
        }
        self.notices.borrow_mut().clear();
        self.unread.set(false);
        self.changed.signal(());
    }

    /// Every notice: questions first, then the newest first.
    #[must_use]
    pub fn list(&self) -> Vec<Notice> {
        let notices = self.notices.borrow();
        notices
            .iter()
            .rev()
            .filter(|notice| notice.question)
            .chain(notices.iter().rev().filter(|notice| !notice.question))
            .cloned()
            .collect()
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

fn label(text: &str, default: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        String::from(default)
    } else {
        text.chars().take(MAX_LABEL_CHARS).collect()
    }
}
