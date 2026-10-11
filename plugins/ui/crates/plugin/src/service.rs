//! The `UiService` capability: opens views and drives them.

use crate::datum::{Datum, Shape};
use crate::mailbox::{Event, Mailbox, Schema};
use crate::package::UiPackage;
use alloc::{collections::BTreeMap, format, rc::Rc, string::String, sync::Arc, vec::Vec};
use barracuda_ui_runtime::{Document, DocumentAssets, DocumentError, format::FormatError};
use barracuda_vm_plugin::{LuaPackageRegistration, LuaPackageRegistry, LuaPackageRegistryError};
use core::cell::{Cell, RefCell};

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
    next_id: Cell<u32>,
    viewport: (f32, f32),
}

impl Inner {
    /// Applies pending Lua writes and lays out every changed view.
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
    /// Creates a service and registers the `ui` Lua package it serves.
    ///
    /// The `ui` Plugin does this during registration; keep the registration
    /// for as long as the package should stay installed in new Lua states.
    ///
    /// # Errors
    ///
    /// Returns the registry's error when the package cannot be registered.
    pub fn register(
        packages: &LuaPackageRegistry,
        viewport: (f32, f32),
    ) -> Result<(Self, LuaPackageRegistration), LuaPackageRegistryError> {
        let mailbox = Arc::new(Mailbox::new());
        let registration = packages.register(UiPackage::new(Arc::clone(&mailbox)))?;
        Ok((Self::new(mailbox, viewport), registration))
    }

    pub(crate) fn new(mailbox: Arc<Mailbox>, viewport: (f32, f32)) -> Self {
        Self {
            inner: Rc::new(Inner {
                mailbox,
                documents: RefCell::new(BTreeMap::new()),
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

    /// Mounts a view and prepares it for one Lua run.
    ///
    /// Hand [`UiSession::ticket`] to the run; it claims the view with
    /// `require("ui").attach(ticket)`. The view lives until the session drops.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the program or an asset is rejected.
    pub fn open(&self, assets: &UiAssets) -> Result<UiSession, UiError> {
        let program = barracuda_ui_runtime::format::read(&assets.view).map_err(UiError::View)?;
        let fonts: Vec<&[u8]> = assets.fonts.iter().map(Vec::as_slice).collect();
        let (width, height) = self.inner.viewport;
        let document = Document::new(
            program,
            DocumentAssets {
                styles: &assets.styles,
                fonts: &fonts,
            },
            width,
            height,
        )
        .map_err(UiError::Document)?;
        let schema = Arc::new(Schema {
            slots: document
                .program()
                .slots
                .iter()
                .map(|slot| (slot.name.clone(), Shape::from(&slot.ty)))
                .collect(),
        });
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id.wrapping_add(1).max(1));
        let ticket = format!("view-{id}");
        self.inner.mailbox.insert(id, ticket.clone(), schema);
        self.inner.documents.borrow_mut().insert(id, document);
        Ok(UiSession {
            id,
            ticket,
            inner: Rc::clone(&self.inner),
        })
    }

    /// Applies Lua writes to open views as they arrive; never returns.
    ///
    /// Sessions also apply pending writes before every tap and read, so a
    /// host without this loop still sees current views.
    pub async fn drive(&self) {
        loop {
            self.inner.mailbox.wait().await;
            self.inner.pump();
        }
    }

    /// Ends every open view for Lua.
    pub(crate) fn close_all(&self) {
        self.inner.mailbox.close_all();
    }
}

/// One open view; dropping it unmounts the view and ends it for Lua.
pub struct UiSession {
    id: u32,
    ticket: String,
    inner: Rc<Inner>,
}

impl UiSession {
    /// The one-time ticket a Lua run passes to `ui.attach`.
    #[must_use]
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Whether Lua closed the view or the `ui` package was revoked.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.mailbox.is_closed(self.id)
    }

    /// Taps the view at (`x`, `y`) and delivers the handler call to Lua.
    ///
    /// Returns the handler name, or `None` when nothing pressable is there or
    /// the event could not be queued.
    pub fn press_at(&self, x: f32, y: f32) -> Option<String> {
        self.inner.pump();
        let event = {
            let mut documents = self.inner.documents.borrow_mut();
            let document = documents.get_mut(&self.id)?;
            let event = document.press_at(x, y)?;
            let handler = document.program().handlers.get(event.handler)?.name.clone();
            let args = event
                .args
                .iter()
                .map(Datum::from_value)
                .collect::<Option<Vec<_>>>()?;
            Event { handler, args }
        };
        let handler = event.handler.clone();
        if !self.inner.mailbox.post(self.id, event) {
            log::warn!(
                "ui view {}: dropped '{handler}' because the view is closed or busy",
                self.id
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
            .get(&self.id)
            .map(Document::texts)
            .unwrap_or_default()
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        self.inner.mailbox.remove(self.id);
        self.inner.documents.borrow_mut().remove(&self.id);
    }
}
