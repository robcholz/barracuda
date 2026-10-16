//! A linked view program: names resolved to slots, ready to run.

use crate::value::{Type, Value};
use alloc::{boxed::Box, string::String, vec::Vec};

/// The most element, branch and list nodes one program may contain.
pub const MAX_NODES: usize = 4096;

/// An expression evaluated against the value slots and loop items.
#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    /// A constant.
    Literal(Value),
    /// A value slot of the program.
    Slot(u16),
    /// A loop item; 0 is the outermost enclosing loop.
    Local(u16),
    /// A struct field by position.
    Field(Box<Expr>, u16),
    /// Logical negation.
    Not(Box<Expr>),
    /// Arithmetic negation.
    Neg(Box<Expr>),
    /// A binary operation.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `condition ? consequent : alternate`.
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A numeric conversion.
    Cast(Cast, Box<Expr>),
}

/// Binary operators supported by compiled views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BinaryOp {
    /// `+`
    Add = 0,
    /// `-`
    Sub = 1,
    /// `*`
    Mul = 2,
    /// `<`
    Lt = 3,
    /// `<=`
    Le = 4,
    /// `>`
    Gt = 5,
    /// `>=`
    Ge = 6,
    /// `===`
    Eq = 7,
    /// `!==`
    Ne = 8,
    /// `&&`
    And = 9,
    /// `||`
    Or = 10,
}

impl BinaryOp {
    pub(crate) fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Lt,
            4 => Self::Le,
            5 => Self::Gt,
            6 => Self::Ge,
            7 => Self::Eq,
            8 => Self::Ne,
            9 => Self::And,
            10 => Self::Or,
            _ => return None,
        })
    }
}

/// Numeric conversion targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Cast {
    /// To `f32` (also `Px`).
    F32 = 0,
    /// To `f64`.
    F64 = 1,
    /// To `i32`, truncating.
    I32 = 2,
}

/// One piece of an element's text.
#[derive(Clone, PartialEq, Debug)]
pub enum TextPart {
    /// Literal text.
    Literal(String),
    /// A formatted expression.
    Expr(Expr),
}

/// A view node.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    /// A `View`, `Text` or `Image` element.
    Element(Element),
    /// A chain of conditional branches; the first matching branch is shown.
    If(Vec<Branch>),
    /// A keyed list.
    For(Box<ForEach>),
}

/// A rendered element.
#[derive(Clone, PartialEq, Debug)]
pub struct Element {
    /// The PocketJS node kind: 0 `View`, 1 `Text`, 2 `Image`.
    pub kind: u8,
    /// The compiled style id.
    pub style: i32,
    /// A style id chosen at runtime.
    pub dynamic_style: Option<Expr>,
    /// Numeric properties as (property id, value).
    pub props: Vec<(u8, Expr)>,
    /// Text that never changes.
    pub static_text: Option<String>,
    /// Text built from literals and expressions; empty when there is none.
    pub text: Vec<TextPart>,
    /// Whether the element can take focus and receive presses.
    pub focusable: bool,
    /// The handler called on press, with its argument expressions.
    pub press: Option<(u16, Vec<Expr>)>,
    /// Child nodes.
    pub children: Vec<Node>,
}

/// One branch of an [`Node::If`].
#[derive(Clone, PartialEq, Debug)]
pub struct Branch {
    /// The branch condition; `None` is the final `else`.
    pub condition: Option<Expr>,
    /// The branch's nodes.
    pub children: Vec<Node>,
}

/// A keyed list.
#[derive(Clone, PartialEq, Debug)]
pub struct ForEach {
    /// The array to iterate.
    pub source: Expr,
    /// The row key, evaluated with the row item as the innermost local.
    pub key: Expr,
    /// The nodes of one row.
    pub children: Vec<Node>,
}

/// A named value slot.
#[derive(Clone, PartialEq, Debug)]
pub struct Slot {
    /// The slot's name in the view contract.
    pub name: String,
    /// The slot's type.
    pub ty: Type,
}

/// A named event handler.
#[derive(Clone, PartialEq, Debug)]
pub struct Handler {
    /// The handler's name in the view contract.
    pub name: String,
    /// Parameter types in order.
    pub params: Vec<Type>,
}

/// A linked view program.
#[derive(Clone, PartialEq, Debug)]
pub struct Program {
    /// The root nodes.
    pub nodes: Vec<Node>,
    /// Value slots, addressed by [`Expr::Slot`].
    pub slots: Vec<Slot>,
    /// Handlers, addressed by [`Element::press`].
    pub handlers: Vec<Handler>,
}

impl Program {
    /// The index of the value slot called `name`.
    #[must_use]
    pub fn slot(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|slot| slot.name == name)
    }

    /// The index of the handler called `name`.
    #[must_use]
    pub fn handler(&self, name: &str) -> Option<usize> {
        self.handlers
            .iter()
            .position(|handler| handler.name == name)
    }
}
