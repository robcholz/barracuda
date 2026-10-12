use alloc::{string::String, vec::Vec};
use core::fmt;
use getset::CopyGetters;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Create,
    Load,
    Memory,
    Runtime,
    Conversion,
    UnexpectedYield,
}

#[derive(Clone, CopyGetters, Debug, Eq, PartialEq)]
pub struct Error {
    #[getset(get_copy = "pub")]
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message)
    }

    /// Reports that a Lua state or a native buffer exceeded its memory budget.
    pub fn memory(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Memory, message)
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn from_lua_bytes(kind: ErrorKind, bytes: Vec<u8>) -> Self {
        let message = String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
        Self::new(kind, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for Error {}
