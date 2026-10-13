//! The owner's side of a view: a `Send + Sync` channel to the UI task.

use crate::datum::{ViewType, ViewValue};
use crate::mailbox::{Mailbox, MailboxError, NextEvent, Schema};
use alloc::{string::String, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use portable_atomic_util::Arc;

/// The longest array one value may hold.
pub const MAX_ARRAY_ITEMS: usize = 1024;

/// One handler call from a view.
pub struct ViewCall {
    /// The handler name from the view contract.
    pub handler: String,
    /// Its arguments, in declaration order.
    pub args: Vec<ViewValue>,
}

/// Why a write to a view was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewError {
    /// The view contract declares no value of that name.
    UnknownValue,
    /// The value does not have the declared type.
    Mismatch,
    /// The view is closed.
    Closed,
}

/// A view as its owner sees it: write values, wait for handler calls.
///
/// Cloning shares the same view. Closing it ends the view for the owner; the
/// UI keeps the session until the owner drops it.
#[derive(Clone)]
pub struct ViewChannel {
    pub(crate) id: u32,
    pub(crate) schema: Arc<Schema>,
    pub(crate) mailbox: Arc<Mailbox>,
}

impl ViewChannel {
    /// The type the view contract declares for the value `name`.
    #[must_use]
    pub fn value_type(&self, name: &str) -> Option<&ViewType> {
        self.schema
            .slots
            .iter()
            .find_map(|(slot, ty)| (slot == name).then_some(ty))
    }

    /// Writes a value declared by the view contract.
    ///
    /// # Errors
    ///
    /// Fails when the view has no such value, `value` does not have its type,
    /// or the view is closed.
    pub fn set(&self, name: &str, value: ViewValue) -> Result<(), ViewError> {
        let (slot, ty) = self
            .schema
            .slots
            .iter()
            .enumerate()
            .find_map(|(index, (slot, ty))| (slot == name).then_some((index, ty)))
            .ok_or(ViewError::UnknownValue)?;
        if !value.fits(ty) {
            return Err(ViewError::Mismatch);
        }
        self.mailbox
            .write(self.id, slot, value)
            .map_err(|error| match error {
                MailboxError::Closed => ViewError::Closed,
                MailboxError::UnknownSlot => ViewError::UnknownValue,
            })
    }

    /// Waits for the next handler call; `None` once the view is closed.
    #[must_use]
    pub fn next(&self) -> ViewNext {
        ViewNext {
            inner: Mailbox::next(&self.mailbox, self.id),
        }
    }

    /// Whether the view still takes writes and delivers calls.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.mailbox.is_closed(self.id)
    }

    /// Ends the view for its owner.
    pub fn close(&self) {
        self.mailbox.close(self.id);
    }
}

/// The future behind [`ViewChannel::next`].
pub struct ViewNext {
    inner: NextEvent,
}

impl Future for ViewNext {
    type Output = Option<ViewCall>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx).map(|event| {
            event.map(|event| ViewCall {
                handler: event.handler,
                args: event.args,
            })
        })
    }
}
