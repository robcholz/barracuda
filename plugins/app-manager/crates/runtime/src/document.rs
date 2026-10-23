//! One mounted view and its values: the unit an app drives.

use crate::program::Program;
use crate::scroll::Scroller;
use crate::value::{Value, ValueError};
use crate::view::View;
use alloc::{string::String, vec::Vec};
use microts::pocketjs_core::Ui as CoreUi;
use microts::{Input, NodeId, Ui};

/// The byte of a baked font atlas that names its slot.
const FONT_SLOT_BYTE: usize = 12;
/// The slot the PocketJS compiler bakes for text without a size or weight
/// class: 16 px regular.
const DEFAULT_FONT_SLOT: u8 = 2;

/// Compiled resources a view needs besides its program.
#[derive(Clone, Copy, Debug)]
pub struct DocumentAssets<'a> {
    /// The compiled stylesheet (`styles.bin`).
    pub styles: &'a [u8],
    /// Baked font atlases (`font-<slot>.bin`).
    pub fonts: &'a [&'a [u8]],
}

/// Why a document could not be created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocumentError {
    /// The stylesheet was rejected by the UI core.
    Styles,
    /// The font atlas at this index was rejected by the UI core.
    Font(usize),
    /// The viewport has no area.
    Viewport,
}

/// A handler call produced by input.
#[derive(Clone, PartialEq, Debug)]
pub struct ViewEvent {
    /// The handler index in [`Program::handlers`].
    pub handler: usize,
    /// The evaluated arguments.
    pub args: Vec<Value>,
}

/// A program mounted into its own UI tree.
pub struct Document {
    ui: Ui,
    program: Program,
    view: View,
    slots: Vec<Value>,
    dirty: bool,
    /// The scroll container a finger holds or that still moves.
    scroll: Option<Scroller>,
}

impl Document {
    /// Mounts `program` into a new UI tree of `width` × `height` pixels.
    ///
    /// Every slot starts at its type's default value.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentError`] when the viewport is empty or an asset is
    /// rejected by the UI core.
    pub fn new(
        program: Program,
        assets: DocumentAssets<'_>,
        width: f32,
        height: f32,
    ) -> Result<Self, DocumentError> {
        if !(width > 0.0 && height > 0.0) {
            return Err(DocumentError::Viewport);
        }
        let mut ui = Ui::new();
        ui.core_mut().set_viewport(width, height);
        // Text wraps to the width its box is given, as on a web page.
        ui.core_mut().set_soft_wrap(true);
        if !ui.load_styles(assets.styles) {
            return Err(DocumentError::Styles);
        }
        for (index, font) in assets.fonts.iter().enumerate() {
            if !ui.core_mut().load_font_atlas(font) {
                return Err(DocumentError::Font(index));
            }
        }
        // Text without a size or weight class carries no font slot, so the UI
        // core draws it with slot 0, while the PocketJS compiler means its
        // 16 px regular default and bakes that as slot 2. Serve slot 0 from the
        // default atlas when the view ships no atlas of its own for it.
        let slot = |font: &&[u8]| font.get(FONT_SLOT_BYTE).copied();
        if !assets.fonts.iter().any(|font| slot(font) == Some(0))
            && let Some(index) = assets
                .fonts
                .iter()
                .position(|font| slot(font) == Some(DEFAULT_FONT_SLOT))
        {
            let mut unstyled = assets.fonts[index].to_vec();
            unstyled[FONT_SLOT_BYTE] = 0;
            if !ui.core_mut().load_font_atlas(&unstyled) {
                return Err(DocumentError::Font(index));
            }
        }
        let slots = program
            .slots
            .iter()
            .map(|slot| slot.ty.default_value())
            .collect();
        let view = View::mount(&mut ui, &program);
        let mut document = Self {
            ui,
            program,
            view,
            slots,
            dirty: true,
            scroll: None,
        };
        document.frame();
        Ok(document)
    }

    /// The running program.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The UI tree, for rendering and inspection.
    #[must_use]
    pub fn ui(&self) -> &Ui {
        &self.ui
    }

    /// The current value of slot `index`.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<&Value> {
        self.slots.get(index)
    }

    /// Writes slot `index`; the view catches up on the next [`Self::frame`].
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::UnknownName`] for an unknown slot and
    /// [`ValueError::TypeMismatch`] when `value` does not match its type.
    pub fn set(&mut self, index: usize, value: Value) -> Result<(), ValueError> {
        let ty = &self
            .program
            .slots
            .get(index)
            .ok_or(ValueError::UnknownName)?
            .ty;
        ty.check(&value)?;
        let slot = self.slots.get_mut(index).ok_or(ValueError::UnknownName)?;
        if *slot != value {
            *slot = value;
            self.dirty = true;
        }
        Ok(())
    }

    /// Whether a value changed since the last frame.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Brings the tree up to date with the values and steps layout,
    /// animation and scrolling.
    pub fn frame(&mut self) {
        if self.dirty {
            self.view.update(&mut self.ui, &self.program, &self.slots);
            self.dirty = false;
        }
        self.ui.tick();
        if let Some(scroll) = self.scroll.as_mut()
            && !scroll.step(self.ui.core_mut())
        {
            self.scroll = None;
        }
    }

    /// Whether the view moves on its own, so it needs frames though nothing
    /// changed: scrolled content carries on or its indicator fades.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.scroll.is_some()
    }

    /// A finger at (`x`, `y`) started to drag vertically: takes hold of the
    /// scroll container there. Returns whether there is one; without one the
    /// drag is not the view's.
    pub fn drag_start(&mut self, x: f32, y: f32) -> bool {
        let core = self.ui.core_mut();
        let Some(target) = core.scroll_target_at(x, y) else {
            return false;
        };
        match self.scroll.as_mut() {
            Some(scroll) if scroll.target() == target => scroll.hold(core),
            _ => {
                if let Some(old) = self.scroll.take() {
                    core.set_scroll_bar(old.target(), 0.0);
                }
                self.scroll = Scroller::grab(core, x, y);
            }
        }
        self.scroll.is_some()
    }

    /// The dragging finger moved `dy` px down since the last frame, 0 when it
    /// held still.
    pub fn drag_by(&mut self, dy: f32) {
        if let Some(scroll) = self.scroll.as_mut() {
            scroll.drag(self.ui.core_mut(), dy);
        }
    }

    /// The dragging finger lifted: the content carries on and settles.
    pub fn drag_end(&mut self) {
        if let Some(scroll) = self.scroll.as_mut() {
            scroll.release();
        }
    }

    /// Draws the tree into `words`, the UI core's DrawList, and returns the
    /// core, which resolves the list's fonts and textures for the rasterizer.
    pub fn draw(&mut self, words: &mut Vec<u32>) -> &CoreUi {
        let list = self.ui.core_mut().draw();
        words.clear();
        words.extend_from_slice(&list.words);
        self.ui.core()
    }

    /// Applies one input sample and returns the handler it selects.
    pub fn input(&mut self, input: &Input) -> Option<ViewEvent> {
        let resolved = self.ui.resolve_input(input);
        let (handler, args) = self.view.dispatch(&self.program, &resolved, &self.slots)?;
        Some(ViewEvent {
            handler: usize::from(handler),
            args,
        })
    }

    /// Presses the focusable element under (`x`, `y`) and releases it.
    pub fn press_at(&mut self, x: f32, y: f32) -> Option<ViewEvent> {
        let mut id = self.ui.core_mut().hit_test(x, y);
        while id != 0 && !self.view.is_pressable(NodeId(id)) {
            id = self.ui.core().node_parent(id);
        }
        if id == 0 {
            return None;
        }
        let event = self.input(&Input::press(NodeId(id)));
        self.ui.resolve_input(&Input::default());
        event
    }

    /// The text of every text-bearing node, in document order.
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.view.collect_texts(&self.ui, &mut out);
        out
    }
}
