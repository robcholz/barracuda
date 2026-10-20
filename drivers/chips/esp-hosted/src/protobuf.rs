//! The subset of the protobuf wire format the RPC messages use.
//!
//! ESP-Hosted encodes its `Rpc` messages with protobuf-c from
//! `esp_hosted_rpc.proto` (proto3). Only varint scalars (`int32`, `uint32`,
//! `uint64`, `bool`, enums), `bytes`, and nested messages occur, so a small
//! writer and a field iterator cover them without generated code. As proto3
//! requires, the writer leaves scalar fields at their default out, while a
//! nested message is always written, even empty, because its presence selects
//! a `oneof` case.

use alloc::vec::Vec;
use core::fmt;

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_LENGTH: u8 = 2;
const WIRE_FIXED32: u8 = 5;

/// Builds one protobuf message.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// Creates an empty message.
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Returns the encoded message.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn key(&mut self, field: u32, wire: u8) {
        push_varint(&mut self.bytes, (u64::from(field) << 3) | u64::from(wire));
    }

    /// Writes an unsigned varint field unless it is zero.
    pub fn uint(&mut self, field: u32, value: u64) -> &mut Self {
        if value != 0 {
            self.key(field, WIRE_VARINT);
            push_varint(&mut self.bytes, value);
        }
        self
    }

    /// Writes an `int32` field unless it is zero; negative values take ten
    /// bytes, as protobuf sign-extends them.
    pub fn int(&mut self, field: u32, value: i32) -> &mut Self {
        if value != 0 {
            self.key(field, WIRE_VARINT);
            push_varint(&mut self.bytes, i64::from(value) as u64);
        }
        self
    }

    /// Writes a `bool` field unless it is false.
    pub fn boolean(&mut self, field: u32, value: bool) -> &mut Self {
        self.uint(field, u64::from(value))
    }

    /// Writes a `bytes` field unless it is empty.
    pub fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        if !value.is_empty() {
            self.key(field, WIRE_LENGTH);
            push_varint(&mut self.bytes, value.len() as u64);
            self.bytes.extend_from_slice(value);
        }
        self
    }

    /// Writes a nested message, even an empty one.
    pub fn message(&mut self, field: u32, build: impl FnOnce(&mut Writer)) -> &mut Self {
        let mut nested = Writer::new();
        build(&mut nested);
        self.key(field, WIRE_LENGTH);
        push_varint(&mut self.bytes, nested.bytes.len() as u64);
        self.bytes.extend_from_slice(&nested.bytes);
        self
    }
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// One decoded field value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    /// A varint scalar.
    Varint(u64),
    /// A length-delimited field: `bytes`, a string, or a nested message.
    Bytes(&'a [u8]),
    /// A 32-bit fixed field.
    Fixed32(u32),
    /// A 64-bit fixed field.
    Fixed64(u64),
}

impl<'a> Value<'a> {
    /// Returns the value as an unsigned integer, zero for other wire types.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        match self {
            Self::Varint(value) | Self::Fixed64(value) => value,
            Self::Fixed32(value) => u64::from(value),
            Self::Bytes(_) => 0,
        }
    }

    /// Returns the value as an `int32`, truncating as protobuf does.
    #[must_use]
    pub fn as_i32(self) -> i32 {
        self.as_u64() as i32
    }

    /// Returns the value as a `uint32`.
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.as_u64() as u32
    }

    /// Returns a length-delimited value, empty for other wire types.
    #[must_use]
    pub fn as_bytes(self) -> &'a [u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            _ => &[],
        }
    }
}

/// Iterates the fields of one encoded message.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Reads `bytes` as a message.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.rest.split_first().ok_or(DecodeError::Truncated)?;
            self.rest = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(DecodeError::VarintTooLong)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        if self.rest.len() < length {
            return Err(DecodeError::Truncated);
        }
        let (taken, rest) = self.rest.split_at(length);
        self.rest = rest;
        Ok(taken)
    }

    fn field(&mut self) -> Result<(u32, Value<'a>), DecodeError> {
        let key = self.varint()?;
        let field = u32::try_from(key >> 3).map_err(|_| DecodeError::BadField)?;
        if field == 0 {
            return Err(DecodeError::BadField);
        }
        let value = match (key & 0x07) as u8 {
            WIRE_VARINT => Value::Varint(self.varint()?),
            WIRE_FIXED64 => {
                let bytes = self.take(8)?;
                let mut word = [0; 8];
                word.copy_from_slice(bytes);
                Value::Fixed64(u64::from_le_bytes(word))
            }
            WIRE_LENGTH => {
                let length = usize::try_from(self.varint()?).map_err(|_| DecodeError::Truncated)?;
                Value::Bytes(self.take(length)?)
            }
            WIRE_FIXED32 => {
                let bytes = self.take(4)?;
                let mut word = [0; 4];
                word.copy_from_slice(bytes);
                Value::Fixed32(u32::from_le_bytes(word))
            }
            other => return Err(DecodeError::WireType(other)),
        };
        Ok((field, value))
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u32, Value<'a>), DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let field = self.field();
        if field.is_err() {
            self.rest = &[];
        }
        Some(field)
    }
}

/// A protobuf message is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The message ends inside a field.
    Truncated,
    /// A varint runs past ten bytes.
    VarintTooLong,
    /// A field number is zero or too large.
    BadField,
    /// A wire type no proto3 message uses here (groups).
    WireType(u8),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("protobuf message is truncated"),
            Self::VarintTooLong => formatter.write_str("protobuf varint is too long"),
            Self::BadField => formatter.write_str("protobuf field number is invalid"),
            Self::WireType(wire) => write!(formatter, "protobuf wire type {wire} is unsupported"),
        }
    }
}

impl core::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn scalars_follow_the_protobuf_encoding() {
        let mut writer = Writer::new();
        writer
            .uint(1, 1)
            .uint(2, 300)
            .int(3, -1)
            .boolean(4, true)
            .uint(5, 0)
            .bytes(6, b"ab")
            .bytes(7, b"");
        assert_eq!(
            writer.finish(),
            [
                0x08, 0x01, // field 1 = 1
                0x10, 0xac, 0x02, // field 2 = 300
                0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0x01, // field 3 = -1
                0x20, 0x01, // field 4 = true
                0x32, 0x02, b'a', b'b', // field 6 = "ab"
            ]
        );
    }

    #[test]
    fn an_empty_nested_message_still_selects_its_field() {
        let mut writer = Writer::new();
        writer.message(280, |_| {});
        // Field 280, wire type 2: key 280 << 3 | 2 = 2242 = 0xc2 0x11.
        assert_eq!(writer.finish(), [0xc2, 0x11, 0x00]);
    }

    #[test]
    fn the_reader_walks_every_wire_type() {
        let bytes = [
            0x08, 0x96, 0x01, // 1: varint 150
            0x12, 0x02, 0x68, 0x69, // 2: bytes "hi"
            0x1d, 0x01, 0x00, 0x00, 0x00, // 3: fixed32 1
            0x21, 2, 0, 0, 0, 0, 0, 0, 0, // 4: fixed64 2
        ];
        let fields: Vec<_> = Reader::new(&bytes).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            fields,
            [
                (1, Value::Varint(150)),
                (2, Value::Bytes(b"hi")),
                (3, Value::Fixed32(1)),
                (4, Value::Fixed64(2)),
            ]
        );
        assert_eq!(Value::Varint(u64::MAX).as_i32(), -1);
    }

    #[test]
    fn a_truncated_message_ends_the_iteration_with_an_error() {
        let mut reader = Reader::new(&[0x12, 0x05, 0x01]);
        assert_eq!(reader.next(), Some(Err(DecodeError::Truncated)));
        assert_eq!(reader.next(), None);
        assert_eq!(
            Reader::new(&[0x0b]).next(),
            Some(Err(DecodeError::WireType(3)))
        );
    }
}
