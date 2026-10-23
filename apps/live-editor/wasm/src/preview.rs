//! The preview: a view mounted as the device mounts it, drawn into an RGBA
//! framebuffer for a canvas. The web app runs the app's Lua program itself
//! and talks to the view through [`Preview::set`] and [`Preview::press`], in
//! the values Lua would pass, as JSON.

use barracuda_ui_runtime::pocketjs_core::raster::render_scaled;
use barracuda_ui_runtime::{Document, DocumentAssets, Type, Value, format};
use serde_json::{Map, Number, Value as Json};
use std::rc::Rc;

/// The byte of a baked font atlas that names its slot.
const FONT_SLOT_BYTE: usize = 12;
/// The most items a view array takes, as on the device.
const MAX_ARRAY_ITEMS: usize = 1024;
/// How deep a value may nest, as on the device.
const MAX_NESTING: usize = 16;

/// One running view and its last frame.
pub struct Preview {
    document: Document,
    width: u32,
    height: u32,
    words: Vec<u32>,
    drawn: Option<(Vec<u32>, u32)>,
    pixels: Vec<u8>,
}

impl Preview {
    /// Mounts the view at `width` × `height`. The view's own atlases come
    /// first; `system` fills the font slots it ships none for, as the
    /// device's system fonts do.
    ///
    /// # Errors
    ///
    /// Says why, in words, when the program, the stylesheet, a font or the
    /// size is not one the device would take.
    pub fn open(
        program: &[u8],
        styles: &[u8],
        own: &[&[u8]],
        system: &[&[u8]],
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let program = format::read(program).map_err(|_| String::from("view.bvir is not BVIR"))?;
        let slot = |font: &[u8]| font.get(FONT_SLOT_BYTE).copied();
        let taken: Vec<Option<u8>> = own.iter().map(|font| slot(font)).collect();
        let mut fonts: Vec<&[u8]> = own.to_vec();
        fonts.extend(
            system
                .iter()
                .copied()
                .filter(|font| !taken.contains(&slot(font))),
        );
        let document = Document::new(
            program,
            DocumentAssets {
                styles,
                fonts: &fonts,
            },
            width as f32,
            height as f32,
        )
        .map_err(|error| match error {
            barracuda_ui_runtime::DocumentError::Styles => {
                String::from("styles.bin is not a stylesheet")
            }
            barracuda_ui_runtime::DocumentError::Font(index) => {
                format!("font atlas {} is not a font atlas", index + 1)
            }
            barracuda_ui_runtime::DocumentError::Viewport => {
                String::from("the display has no area")
            }
        })?;
        Ok(Self {
            document,
            width,
            height,
            words: Vec::new(),
            drawn: None,
            pixels: Vec::new(),
        })
    }

    /// Writes the view value `name`, converted from JSON as `view:set`
    /// converts a Lua value; the error is the one `view:set` returns.
    ///
    /// # Errors
    ///
    /// Returns the device's message when the view has no such value or
    /// `value` does not fit its type.
    pub fn set(&mut self, name: &str, value: &Json) -> Result<(), String> {
        let unknown = || format!("the view has no value named '{name}'");
        let index = self.document.program().slot(name).ok_or_else(unknown)?;
        let ty = &self.document.program().slots[index].ty;
        let value =
            to_value(value, ty, 0).map_err(|expected| format!("'{name}': expected {expected}"))?;
        self.document.set(index, value).map_err(|_| unknown())
    }

    /// Steps the view one tick: values, layout and animation.
    pub fn frame(&mut self) {
        self.document.frame();
    }

    /// Ticks per second the view's animations are timed in.
    #[must_use]
    pub fn tick_rate(&self) -> u32 {
        self.document.ui().core().tick_rate()
    }

    /// Draws the view at `scale` physical pixels per view pixel and answers
    /// the RGBA pixels, or `None` when the picture is the one drawn last.
    pub fn draw(&mut self, scale: u32) -> Option<&[u8]> {
        let scale = scale.clamp(1, 4);
        let core = self.document.draw(&mut self.words);
        if self
            .drawn
            .as_ref()
            .is_some_and(|(words, drawn)| *drawn == scale && *words == self.words)
        {
            return None;
        }
        let pixels = (self.width * scale) as usize * (self.height * scale) as usize * 4;
        self.pixels.resize(pixels, 0);
        render_scaled(core, &self.words, &mut self.pixels, scale);
        self.drawn = Some((self.words.clone(), scale));
        Some(&self.pixels)
    }

    /// Taps (`x`, `y`) in view pixels and answers the handler call it
    /// selects, as the event Lua receives: `{"handler", "args"}`.
    pub fn press(&mut self, x: f32, y: f32) -> Option<Json> {
        let event = self.document.press_at(x, y)?;
        let handler = self.document.program().handlers.get(event.handler)?;
        let mut call = Map::new();
        call.insert("handler".into(), Json::String(handler.name.clone()));
        call.insert(
            "args".into(),
            Json::Array(event.args.iter().map(to_json).collect()),
        );
        Some(Json::Object(call))
    }

    /// A pointer that went down at (`x`, `y`) set off vertically: answers
    /// whether the view scrolls there, and takes the drag if it does.
    pub fn drag_start(&mut self, x: f32, y: f32) -> bool {
        self.document.drag_start(x, y)
    }

    /// The dragging pointer moved `dy` view pixels down since the last frame.
    pub fn drag_by(&mut self, dy: f32) {
        self.document.drag_by(dy);
    }

    /// The dragging pointer lifted: the content carries on and settles.
    pub fn drag_end(&mut self) {
        self.document.drag_end();
    }

    /// The text on screen, in document order.
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.document.texts()
    }

    /// The view's contract: each value with its type, and each handler with
    /// its parameters, in the words of `app.d.ts`.
    #[must_use]
    pub fn contract(&self) -> Json {
        let program = self.document.program();
        let mut values = Map::new();
        for slot in &program.slots {
            values.insert(slot.name.clone(), Json::String(type_name(&slot.ty)));
        }
        let mut handlers = Map::new();
        for handler in &program.handlers {
            handlers.insert(
                handler.name.clone(),
                Json::Array(
                    handler
                        .params
                        .iter()
                        .map(|ty| Json::String(type_name(ty)))
                        .collect(),
                ),
            );
        }
        let mut contract = Map::new();
        contract.insert("values".into(), Json::Object(values));
        contract.insert("handlers".into(), Json::Object(handlers));
        Json::Object(contract)
    }
}

/// Converts JSON, standing for a Lua value, to a view value of type `ty`, by
/// the rules of the device's `view:set`; the error names the expected type.
fn to_value(value: &Json, ty: &Type, depth: usize) -> Result<Value, &'static str> {
    if depth > MAX_NESTING {
        return Err("a shallower table");
    }
    Ok(match (ty, value) {
        (Type::Bool, Json::Bool(value)) => Value::Bool(*value),
        (Type::I32, Json::Number(number)) => Value::I32(integer(number).ok_or("an i32")?),
        (Type::F32, Json::Number(number)) => Value::F32(number.as_f64().unwrap_or(0.0) as f32),
        (Type::F64, Json::Number(number)) => Value::F64(number.as_f64().unwrap_or(0.0)),
        (Type::Str, Json::String(value)) => Value::Str(Rc::from(value.as_str())),
        // An empty Lua table has no way to say whether it is an array.
        (Type::Array(_), Json::Object(map)) if map.is_empty() => Value::Array(Rc::from([])),
        (Type::Array(element), Json::Array(items)) => {
            if items.len() > MAX_ARRAY_ITEMS {
                return Err("at most 1024 items");
            }
            Value::Array(
                items
                    .iter()
                    .map(|item| to_value(item, element, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            )
        }
        (Type::Struct(st), Json::Object(map)) => Value::Struct(
            st.fields
                .iter()
                .map(|(name, ty)| to_value(map.get(name).unwrap_or(&Json::Null), ty, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        (ty, _) => return Err(expected(ty)),
    })
}

/// An integral number in `i32`'s range, as a Lua integer or integral float.
fn integer(number: &Number) -> Option<i32> {
    if let Some(value) = number.as_i64() {
        return i32::try_from(value).ok();
    }
    let value = number.as_f64()?;
    let truncated = value as i32;
    (f64::from(truncated) == value).then_some(truncated)
}

fn expected(ty: &Type) -> &'static str {
    match ty {
        Type::Bool => "a boolean",
        Type::I32 => "an integer",
        Type::F32 | Type::F64 => "a number",
        Type::Str => "a string",
        Type::Array(_) => "an array table",
        Type::Struct(_) => "a table",
    }
}

/// A handler argument as Lua gets it: scalars as they are, lists as `nil`.
fn to_json(value: &Value) -> Json {
    match value {
        Value::Bool(value) => Json::Bool(*value),
        Value::I32(value) => Json::from(*value),
        Value::F32(value) => Number::from_f64(f64::from(*value)).map_or(Json::Null, Json::Number),
        Value::F64(value) => Number::from_f64(*value).map_or(Json::Null, Json::Number),
        Value::Str(value) => Json::String(String::from(&**value)),
        Value::Void | Value::Struct(_) | Value::Array(_) => Json::Null,
    }
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Bool => String::from("boolean"),
        Type::I32 => String::from("i32"),
        Type::F32 => String::from("f32"),
        Type::F64 => String::from("number"),
        Type::Str => String::from("string"),
        Type::Array(element) => format!("{}[]", type_name(element)),
        Type::Struct(st) => st.name.clone(),
    }
}
