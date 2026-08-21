//! Field-level wire access over fixed-layout typed RPC frames.
//!
//! [`WireField`] names a byte region by its JSON field name, [`RpcWire`] is
//! the per-struct table of those regions (emitted by `#[derive(RpcWire)]`),
//! and [`WireSupport`] bundles a method's request-write and response-read
//! tables.
//!
//! The framework treats every region as opaque bytes. It never interprets a
//! field's contents: length prefixes, encodings, and multi-field logical values
//! are the caller's responsibility. [`WireSupport::read_response_field`] returns
//! the full region; [`WireSupport::write_request_field`] performs a bounded copy
//! (`content.len() <= region size`) and nothing more.

use alloc::string::ToString;

use super::typed::{RpcMessage, RpcMethod};
use super::{RpcError, RpcResult};

/// One named byte region within a fixed-layout message struct.
///
/// Emitted by `#[derive(RpcWire)]`; the `name` is the field's JSON name (after
/// any `#[serde(rename)]`/`rename_all`), and `offset`/`size` come from
/// `offset_of!`/`size_of` at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireField {
    name: &'static str,
    offset: usize,
    size: usize,
}

impl WireField {
    /// Records one field's JSON name and byte region.
    #[must_use]
    pub const fn new(name: &'static str, offset: usize, size: usize) -> Self {
        Self { name, offset, size }
    }

    /// Returns the field's JSON name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the field's byte size.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the field's byte region as `start..end`, guarding overflow.
    fn range(&self) -> RpcResult<(usize, usize)> {
        let end = self
            .offset
            .checked_add(self.size)
            .ok_or(RpcError::InvalidFrameState)?;
        Ok((self.offset, end))
    }
}

/// Compile-time field→region table for one fixed-layout message struct.
///
/// Derive it with `#[derive(RpcWire)]`. Message structs that carry no
/// addressable fields — notably `()` — expose an empty table.
pub trait RpcWire: RpcMessage {
    /// The struct's named fields, in declaration order.
    const FIELDS: &'static [WireField];
}

impl RpcWire for () {
    const FIELDS: &'static [WireField] = &[];
}

/// A method's request-write and response-read field tables.
///
/// Built by [`WireSupport::of`] where the concrete method type is known, then
/// stored beside the endpoint through [`RpcMethod::dynamic`]. Callers read
/// fields from the response table and write them into the request table.
#[derive(Clone, Copy, Debug)]
pub struct WireSupport {
    request: &'static [WireField],
    response: &'static [WireField],
}

impl WireSupport {
    /// Builds the wire tables for method `M`.
    ///
    /// `#[rpc_dynamic]` emits this call; write it by hand only to fill
    /// [`RpcMethod::dynamic`] without the attribute.
    #[must_use]
    pub fn of<M>() -> Self
    where
        M: RpcMethod,
        M::Request: RpcWire,
        M::Response: RpcWire,
    {
        Self {
            request: <M::Request as RpcWire>::FIELDS,
            response: <M::Response as RpcWire>::FIELDS,
        }
    }

    /// Returns the full byte region of a response field.
    ///
    /// The region is returned verbatim; the framework does not interpret it.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::WireFieldUnknown`] when no response field carries
    /// `name`, or [`RpcError::WireFieldRegion`] when the frame is shorter than
    /// the field's recorded extent.
    pub fn read_response_field<'a>(&self, region: &'a [u8], name: &str) -> RpcResult<&'a [u8]> {
        let field = locate(self.response, name)?;
        let (start, end) = field.range()?;
        region
            .get(start..end)
            .ok_or_else(|| RpcError::WireFieldRegion {
                field: name.to_string(),
            })
    }

    /// Copies `content` into a request field's byte region.
    ///
    /// `content` may be shorter than the region (a narrower source buffer); the
    /// remaining bytes of the region are left untouched. It may not exceed the
    /// region — preserving any length/encoding convention is the caller's job.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::WireFieldUnknown`] when no request field carries
    /// `name`, [`RpcError::WireFieldRegion`] when the request buffer is shorter
    /// than the field's recorded extent, or [`RpcError::WireFieldTooLarge`] when
    /// `content` exceeds the field's size.
    pub fn write_request_field(
        &self,
        region: &mut [u8],
        name: &str,
        content: &[u8],
    ) -> RpcResult<()> {
        let field = locate(self.request, name)?;
        if content.len() > field.size {
            return Err(RpcError::WireFieldTooLarge {
                field: name.to_string(),
                size: content.len(),
                capacity: field.size,
            });
        }
        let (start, _end) = field.range()?;
        let write_end = start
            .checked_add(content.len())
            .ok_or(RpcError::InvalidFrameState)?;
        region
            .get_mut(start..write_end)
            .ok_or_else(|| RpcError::WireFieldRegion {
                field: name.to_string(),
            })?
            .copy_from_slice(content);
        Ok(())
    }

    /// Returns a response field's byte size, or `None` when absent.
    #[must_use]
    pub fn response_field_size(&self, name: &str) -> Option<usize> {
        locate(self.response, name).ok().map(|field| field.size)
    }

    /// Returns a request field's byte size, or `None` when absent.
    #[must_use]
    pub fn request_field_size(&self, name: &str) -> Option<usize> {
        locate(self.request, name).ok().map(|field| field.size)
    }
}

/// Finds a field by JSON name, or reports it unknown.
fn locate(fields: &'static [WireField], name: &str) -> RpcResult<WireField> {
    fields
        .iter()
        .find(|field| field.name == name)
        .copied()
        .ok_or_else(|| RpcError::WireFieldUnknown {
            field: name.to_string(),
        })
}
