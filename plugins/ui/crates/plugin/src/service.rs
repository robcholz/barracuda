//! The `UiService` capability: opens views and drives them.

use crate::datum::{ViewType, ViewValue};
use crate::mailbox::{Event, Mailbox, Schema};
use crate::view::ViewChannel;
use alloc::{collections::BTreeMap, rc::Rc, string::String, vec::Vec};
use barracuda_ui_runtime::{
    Document, DocumentAssets, DocumentError, Program, Value, format::FormatError,
};
use core::cell::{Cell, RefCell};
use portable_atomic_util::Arc;

/// The viewport views are laid out in until a display backend sets one.
pub const DEFAULT_VIEWPORT: (f32, f32) = (360.0, 360.0);

/// The compiled files of one view.
#[derive(Clone, Debug, Default)]
pub struct UiAssets {
    /// The linked view program (`view.bvir`).
    pub view: Vec<u8>,
    /// The compiled stylesheet (`styles.bin`).
    pub styles: Vec<u8>,
    /// Baked font atlases (`font-<slot>.bin`).
    pub fonts: Vec<Vec<u8>>,
}

/// Why a view could not be opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiError {
    /// `view.bvir` is malformed.
    View(FormatError),
    /// The stylesheet or a font was rejected.
    Document(DocumentError),
}

impl core::fmt::Display for UiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UiError::View(error) => write!(f, "invalid view program: {error:?}"),
            UiError::Document(error) => write!(f, "view assets rejected: {error:?}"),
        }
    }
}

impl core::error::Error for UiError {}

struct Inner {
    mailbox: Arc<Mailbox>,
    documents: RefCell<BTreeMap<u32, Document>>,
    /// The values of suspended views, kept to rebuild their trees on resume.
    suspended: RefCell<BTreeMap<u32, Vec<Value>>>,
    next_id: Cell<u32>,
    viewport: (f32, f32),
}

impl Inner {
    fn document(&self, program: Program, assets: &UiAssets) -> Result<Document, UiError> {
        let fonts: Vec<&[u8]> = assets.fonts.iter().map(Vec::as_slice).collect();
        let (width, height) = self.viewport;
        Document::new(
            program,
            DocumentAssets {
                styles: &assets.styles,
                fonts: &fonts,
            },
            width,
            height,
        )
        .map_err(UiError::Document)
    }

    /// Applies pending writes and lays out every changed view.
    fn pump(&self) {
        let mut documents = self.documents.borrow_mut();
        for (id, document) in documents.iter_mut() {
            for (slot, datum) in self.mailbox.take_writes(*id) {
                let value = document
                    .program()
                    .slots
                    .get(slot)
                    .and_then(|slot| datum.into_value(&slot.ty));
                match value.map(|value| document.set(slot, value)) {
                    Some(Ok(())) => {}
                    _ => log::warn!(
                        "ui view {id}: dropped a write to slot {slot} that does not match its type"
                    ),
                }
            }
            if document.is_dirty() {
                document.frame();
            }
        }
    }
}

/// Opens views; published by the `ui` Plugin.
pub struct UiService {
    inner: Rc<Inner>,
}

impl UiService {
    /// Creates a service that lays views out at `viewport`.
    #[must_use]
    pub fn new(viewport: (f32, f32)) -> Self {
        Self {
            inner: Rc::new(Inner {
                mailbox: Arc::new(Mailbox::new()),
                documents: RefCell::new(BTreeMap::new()),
                suspended: RefCell::new(BTreeMap::new()),
                next_id: Cell::new(1),
                viewport,
            }),
        }
    }

    /// The size views are laid out in.
    #[must_use]
    pub fn viewport(&self) -> (f32, f32) {
        self.inner.viewport
    }

    /// Mounts a view. It lives until the session drops.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the program or an asset is rejected.
    pub fn open(&self, assets: &UiAssets) -> Result<UiSession, UiError> {
        let program = barracuda_ui_runtime::format::read(&assets.view).map_err(UiError::View)?;
        let document = self.inner.document(program, assets)?;
        let schema = Arc::new(Schema {
            slots: document
                .program()
                .slots
                .iter()
                .map(|slot| (slot.name.clone(), ViewType::from(&slot.ty)))
                .collect(),
        });
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id.wrapping_add(1).max(1));
        self.inner.mailbox.insert(id, schema.slots.len());
        self.inner.documents.borrow_mut().insert(id, document);
        Ok(UiSession {
            channel: ViewChannel {
                id,
                schema,
                mailbox: Arc::clone(&self.inner.mailbox),
            },
            inner: Rc::clone(&self.inner),
        })
    }

    /// Applies writes to open views as they arrive; never returns.
    ///
    /// Sessions also apply pending writes before every tap and read, so a
    /// host without this loop still sees current views.
    pub async fn drive(&self) {
        loop {
            self.inner.mailbox.wait().await;
            self.inner.pump();
        }
    }

    /// Ends every open view for its owner.
    pub(crate) fn close_all(&self) {
        self.inner.mailbox.close_all();
    }
}

/// One open view; dropping it unmounts the view and ends it for its owner.
pub struct UiSession {
    channel: ViewChannel,
    inner: Rc<Inner>,
}

impl UiSession {
    /// The channel the view's owner writes to and reads calls from.
    #[must_use]
    pub fn channel(&self) -> &ViewChannel {
        &self.channel
    }

    /// Whether the owner closed the view.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.mailbox.is_closed(self.channel.id)
    }

    /// Drops handler calls the owner has not received yet.
    pub fn clear_events(&self) {
        self.inner.mailbox.clear_events(self.channel.id);
    }

    /// Whether the tree is released; see [`UiSession::suspend`].
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        !self.inner.documents.borrow().contains_key(&self.channel.id)
    }

    /// Releases the view's tree and keeps only its values.
    ///
    /// A view is a function of its values, so [`UiSession::resume`] rebuilds the
    /// same tree. Writes made meanwhile wait in the channel. Taps and reads
    /// see nothing while suspended.
    pub fn suspend(&self) {
        self.inner.pump();
        let Some(document) = self.inner.documents.borrow_mut().remove(&self.channel.id) else {
            return;
        };
        let values = (0..document.program().slots.len())
            .map(|slot| document.value(slot).cloned().unwrap_or(Value::Void))
            .collect();
        self.inner
            .suspended
            .borrow_mut()
            .insert(self.channel.id, values);
    }

    /// Rebuilds a suspended view from its assets and kept values.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the assets are rejected or no longer match the
    /// view; the session stays suspended.
    pub fn resume(&self, assets: &UiAssets) -> Result<(), UiError> {
        if !self.is_suspended() {
            return Ok(());
        }
        let program = barracuda_ui_runtime::format::read(&assets.view).map_err(UiError::View)?;
        let compatible =
            program.slots.len() == self.channel.schema.slots.len()
                && program.slots.iter().zip(&self.channel.schema.slots).all(
                    |(slot, (name, shape))| {
                        slot.name == *name && ViewType::from(&slot.ty) == *shape
                    },
                );
        if !compatible {
            return Err(UiError::View(FormatError::Invalid(
                "the view changed while suspended",
            )));
        }
        let mut document = self.inner.document(program, assets)?;
        let values = self
            .inner
            .suspended
            .borrow_mut()
            .remove(&self.channel.id)
            .unwrap_or_default();
        for (slot, value) in values.into_iter().enumerate() {
            let _matching = document.set(slot, value);
        }
        document.frame();
        self.inner
            .documents
            .borrow_mut()
            .insert(self.channel.id, document);
        self.inner.pump();
        Ok(())
    }

    /// Taps the view at (`x`, `y`) and delivers the handler call to the owner.
    ///
    /// Returns the handler name, or `None` when nothing pressable is there or
    /// the event could not be queued.
    pub fn press_at(&self, x: f32, y: f32) -> Option<String> {
        self.inner.pump();
        let event = {
            let mut documents = self.inner.documents.borrow_mut();
            let document = documents.get_mut(&self.channel.id)?;
            let event = document.press_at(x, y)?;
            let handler = document.program().handlers.get(event.handler)?.name.clone();
            let args = event
                .args
                .iter()
                .map(ViewValue::from_value)
                .collect::<Option<Vec<_>>>()?;
            Event { handler, args }
        };
        let handler = event.handler.clone();
        if !self.inner.mailbox.post(self.channel.id, event) {
            log::warn!(
                "ui view {}: dropped '{handler}' because the view is closed or busy",
                self.channel.id
            );
            return None;
        }
        Some(handler)
    }

    /// The text of every text-bearing node in document order, after pending writes.
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.inner.pump();
        self.inner
            .documents
            .borrow()
            .get(&self.channel.id)
            .map(Document::texts)
            .unwrap_or_default()
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        self.inner.mailbox.remove(self.channel.id);
        self.inner.documents.borrow_mut().remove(&self.channel.id);
        self.inner.suspended.borrow_mut().remove(&self.channel.id);
    }
}
