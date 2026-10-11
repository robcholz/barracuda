//! Runtime for compiled MicroTS views.
//!
//! A MicroTS view is compiled ahead of time into View IR and linked into a
//! [`Program`]: names become slot indices, struct fields become positions, and
//! every value slot keeps its [`Type`] so callers can convert their own data
//! into [`Value`]s. A program travels as a BVIR file ([`format`]) and runs in a
//! [`Document`], which owns the PocketJS UI tree for one view.

#![no_std]

extern crate alloc;

#[cfg(feature = "compiler")]
pub mod compiler;
mod document;
pub mod format;
mod program;
mod value;
mod view;

pub use document::{Document, DocumentAssets, DocumentError, ViewEvent};
pub use microts::{NodeId, Ui};
pub use program::{
    BinaryOp, Branch, Cast, Element, Expr, ForEach, Handler, MAX_NODES, Node, Program, Slot,
    TextPart,
};
pub use value::{Key, StructType, Type, Value, ValueError};
