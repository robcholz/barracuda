use alloc::{string::String, vec::Vec};
use core::fmt;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
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

    pub fn kind(&self) -> ErrorKind {
        self.kind
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
