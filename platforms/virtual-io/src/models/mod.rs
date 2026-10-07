//! Datasheet models of real chips reached over I2C.
//!
//! Each model cites its datasheet (document, revision, section or table) for
//! every register behaviour, ordering rule, and timing number it checks.

pub mod bq27220;
pub mod ina226;
pub mod pi4ioe5v6408;
pub mod rx8130ce;

pub use bq27220::Bq27220Model;
pub use ina226::Ina226Model;
pub use pi4ioe5v6408::Pi4ioe5v6408Model;
pub use rx8130ce::Rx8130ceModel;
