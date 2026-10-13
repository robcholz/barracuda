//! The `UiService` capability: opens views and drives them.

use crate::datum::{ViewType, ViewValue};
use crate::gesture::Gesture;
use crate::mailbox::{Event, Mailbox, Schema};
use crate::view::{ViewCall, ViewChannel};
use alloc::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    string::String,
    vec::Vec,
};
use barracuda_ui_runtime::{
    Document, DocumentAssets, DocumentError, Program, Value, format::FormatError,
};
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use portable_atomic_util::Arc;

/// The viewport views are laid out in when the Board gives the Plugin no
/// display.
pub const DEFAULT_VIEWPORT: (f32, f32) = (360.0, 360.0);

/// Gestures kept for the system UI before older ones are dropped.
pub const MAX_GESTURES: usize = 8;

/// Where a view sits on the display.
///
/// Each layer shows at most one view. The display shows the system layer's
/// view when it has one, and the app layer's otherwise, so the system UI
/// covers apps without the apps' owner taking their views down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// Apps' views.
    App,
    /// The system UI's views: home, panels, and dialogs.
    System,
}

impl Layer {
    const fn index(self) -> usize {
        match self {
            Layer::App => 0,
            Layer::System => 1,
        }
    }
}

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

/// What the display shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Shown {
    /// The view on screen, if any.
    pub(crate) view: Option<u32>,
    /// Whether touches reach it.
    pub(crate) input: bool,
    /// Counts changes of what is shown, so a redraw can start from scratch.
    pub(crate) revision: u64,
}

/// What one layer shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Placement {
    view: Option<u32>,
    input: bool,
}

struct Inner {
    mailbox: Arc<Mailbox>,
    documents: RefCell<BTreeMap<u32, Document>>,
    /// The values of suspended views, kept to rebuild their trees on resume.
    suspended: RefCell<BTreeMap<u32, Vec<Value>>>,
    next_id: Cell<u32>,
    viewport: (f32, f32),
    layers: Cell<[Placement; 2]>,
    shown: Cell<Shown>,
    gestures: RefCell<VecDeque<Gesture>>,
    gesture: Signal<NoopRawMutex, ()>,
    brightness: Cell<Option<u8>>,
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

    /// Puts `view` on `layer`, or clears the layer, and shows the top one.
    fn place(&self, layer: Layer, view: Option<u32>, input: bool) {
        let mut layers = self.layers.get();
        layers[layer.index()] = Placement { view, input };
        self.layers.set(layers);
        let [app, system] = layers;
        let top = if system.view.is_some() { system } else { app };
        self.show(top.view, top.input);
    }

    fn show(&self, view: Option<u32>, input: bool) {
        let shown = self.shown.get();
        if shown.view == view && shown.input == input {
            return;
        }
        let revision = if shown.view == view {
            shown.revision
        } else {
            shown.revision.wrapping_add(1)
        };
        self.shown.set(Shown {
            view,
            input,
            revision,
        });
        self.mailbox.notify();
    }

    /// Takes view `id` off its layer if it is there.
    fn withdraw(&self, id: u32) {
        for layer in [Layer::App, Layer::System] {
            if self.layers.get()[layer.index()].view == Some(id) {
                self.place(layer, None, false);
            }
        }
    }

    /// The handler call pressing view `id` at (`x`, `y`) selects.
    fn resolve(&self, id: u32, x: f32, y: f32) -> Option<Event> {
        self.pump();
        let mut documents = self.documents.borrow_mut();
        let document = documents.get_mut(&id)?;
        let event = document.press_at(x, y)?;
        let handler = document.program().handlers.get(event.handler)?.name.clone();
        let args = event
            .args
            .iter()
            .map(ViewValue::from_value)
            .collect::<Option<Vec<_>>>()?;
        Some(Event { handler, args })
    }

    /// Presses view `id` at (`x`, `y`) and queues the handler call it selects.
    fn press(&self, id: u32, x: f32, y: f32) -> Option<String> {
        let event = self.resolve(id, x, y)?;
        let handler = event.handler.clone();
        if !self.mailbox.post(id, event) {
            log::warn!("ui view {id}: dropped '{handler}' because the view is closed or busy");
            return None;
        }
        Some(handler)
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
                layers: Cell::new([Placement::default(); 2]),
                shown: Cell::new(Shown::default()),
                gestures: RefCell::new(VecDeque::new()),
                gesture: Signal::new(),
                brightness: Cell::new(None),
            }),
        }
    }

    /// The size views are laid out in.
    #[must_use]
    pub fn viewport(&self) -> (f32, f32) {
        self.inner.viewport
    }

    /// Mounts an app's view on the [`Layer::App`] layer. It lives until the
    /// session drops.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the program or an asset is rejected.
    pub fn open(&self, assets: &UiAssets) -> Result<UiSession, UiError> {
        self.open_on(Layer::App, assets)
    }

    /// Mounts a view on `layer`. It lives until the session drops.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the program or an asset is rejected.
    pub fn open_on(&self, layer: Layer, assets: &UiAssets) -> Result<UiSession, UiError> {
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
            layer,
            channel: ViewChannel {
                id,
                schema,
                mailbox: Arc::clone(&self.inner.mailbox),
            },
            inner: Rc::clone(&self.inner),
        })
    }

    /// What the display shows: `None` when it is blank, otherwise whether
    /// the view on it takes touches.
    #[must_use]
    pub fn showing(&self) -> Option<bool> {
        let shown = self.inner.shown.get();
        shown.view.map(|_| shown.input)
    }

    /// Waits for the next long press or swipe on the display.
    ///
    /// Gestures are the system UI's: taps press the view on the display, and
    /// gestures reach no view. Only the latest [`MAX_GESTURES`] are kept
    /// while nobody waits.
    pub async fn next_gesture(&self) -> Gesture {
        loop {
            if let Some(gesture) = self.inner.gestures.borrow_mut().pop_front() {
                return gesture;
            }
            self.inner.gesture.wait().await;
        }
    }

    /// Sets the display's brightness on the `0..=255` scale.
    pub fn set_brightness(&self, brightness: u8) {
        self.inner.brightness.set(Some(brightness));
        self.inner.mailbox.notify();
    }

    /// Queues a gesture for [`UiService::next_gesture`].
    pub(crate) fn post_gesture(&self, gesture: Gesture) {
        let mut gestures = self.inner.gestures.borrow_mut();
        if gestures.len() == MAX_GESTURES {
            gestures.pop_front();
        }
        gestures.push_back(gesture);
        self.inner.gesture.signal(());
    }

    /// A brightness set since the last call.
    pub(crate) fn take_brightness(&self) -> Option<u8> {
        self.inner.brightness.take()
    }

    /// Waits until a view is written or what is on screen changes.
    pub(crate) async fn changed(&self) {
        self.inner.mailbox.wait().await;
    }

    /// Applies pending writes and lays out every changed view.
    ///
    /// Sessions also apply pending writes before every tap and read, so
    /// callers without the UI task still see current views.
    pub(crate) fn pump(&self) {
        self.inner.pump();
    }

    /// What the display shows.
    pub(crate) fn shown(&self) -> Shown {
        self.inner.shown.get()
    }

    /// Runs `f` on the tree of view `id`; `None` when it has none, such as
    /// while suspended.
    pub(crate) fn with_document<R>(
        &self,
        id: u32,
        f: impl FnOnce(&mut Document) -> R,
    ) -> Option<R> {
        self.inner.documents.borrow_mut().get_mut(&id).map(f)
    }

    /// Presses the view on screen at (`x`, `y`) if it takes input.
    pub(crate) fn press_shown(&self, x: f32, y: f32) -> Option<String> {
        let shown = self.inner.shown.get();
        let view = shown.view.filter(|_| shown.input)?;
        self.inner.press(view, x, y)
    }

    /// Ends every open view for its owner.
    pub(crate) fn close_all(&self) {
        self.inner.mailbox.close_all();
    }
}

/// One open view; dropping it unmounts the view and ends it for its owner.
pub struct UiSession {
    layer: Layer,
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

    /// The layer the view sits on.
    #[must_use]
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// Puts the view on its layer in place of whatever was there; touches
    /// reach it only while `input` is set. It is on the display unless the
    /// system layer covers it.
    pub fn show(&self, input: bool) {
        self.inner.place(self.layer, Some(self.channel.id), input);
    }

    /// Takes the view off its layer if it is there, uncovering the app layer
    /// or leaving the display blank.
    pub fn hide(&self) {
        self.inner.withdraw(self.channel.id);
    }

    /// Whether the view is on the display.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.inner.shown.get().view == Some(self.channel.id)
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
    /// see nothing while suspended, and the view leaves the display.
    pub fn suspend(&self) {
        self.inner.withdraw(self.channel.id);
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

    /// Taps the view at (`x`, `y`) and delivers the handler call to the owner,
    /// whether or not it is on the display.
    ///
    /// Returns the handler name, or `None` when nothing pressable is there or
    /// the event could not be queued.
    pub fn press_at(&self, x: f32, y: f32) -> Option<String> {
        self.inner.press(self.channel.id, x, y)
    }

    /// The handler call a press at (`x`, `y`) would deliver, without
    /// delivering it. The system UI resolves long presses this way.
    #[must_use]
    pub fn resolve_press(&self, x: f32, y: f32) -> Option<ViewCall> {
        self.inner
            .resolve(self.channel.id, x, y)
            .map(|event| ViewCall {
                handler: event.handler,
                args: event.args,
            })
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
        self.inner.withdraw(self.channel.id);
        self.inner.mailbox.remove(self.channel.id);
        self.inner.documents.borrow_mut().remove(&self.channel.id);
        self.inner.suspended.borrow_mut().remove(&self.channel.id);
    }
}
