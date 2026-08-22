use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_event_router::{rpc_message, Event, Streaming};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::route::GatewayRoute;
use crate::wire::{GatewayText, GatewayWireError};

const TEXT_CAPACITY: usize = 508;

/// Gateway-owned logical payload of `gateway.message.received`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayInboundMessage {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// Provider-assigned message identifier.
    pub message_id: String,
    /// User-visible message text.
    pub text: String,
}

/// Semantic field carried by one [`GatewayEventFrame`].
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum GatewayEventField {
    /// Registered message-channel name.
    Channel,
    /// Provider conversation identifier.
    Conversation,
    /// Optional provider thread identifier.
    Thread,
    /// Provider-assigned message identifier.
    MessageId,
    /// More text belongs to the same inbound message.
    TextMore,
    /// This frame completes the inbound message text.
    TextComplete,
}

/// One typed frame carrying part of a `gateway.message.received` Event.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayEventFrame {
    text: GatewayText<TEXT_CAPACITY>,
    field: GatewayEventField,
}

impl GatewayEventFrame {
    fn new(field: GatewayEventField, text: &str) -> Result<Self, GatewayWireError> {
        Ok(Self {
            text: GatewayText::new(text)?,
            field,
        })
    }

    fn text(&self) -> Result<&str, GatewayWireError> {
        self.text.as_str()
    }
}

/// Event emitted for each normalized inbound text message.
pub struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
    type Message = GatewayEventFrame;
    type Input = Streaming;
}

/// Encodes one normalized inbound message into typed Event frames.
///
/// # Errors
///
/// Returns a wire error when route metadata cannot fit in one frame or message
/// text contains a NUL byte.
pub fn frames_from_gateway_event(
    value: &GatewayInboundMessage,
) -> Result<Vec<GatewayEventFrame>, GatewayWireError> {
    let mut frames = Vec::new();
    frames.push(GatewayEventFrame::new(
        GatewayEventField::Channel,
        &value.route.channel,
    )?);
    frames.push(GatewayEventFrame::new(
        GatewayEventField::Conversation,
        &value.route.conversation_id,
    )?);
    if let Some(thread_id) = &value.route.thread_id {
        frames.push(GatewayEventFrame::new(
            GatewayEventField::Thread,
            thread_id,
        )?);
    }
    frames.push(GatewayEventFrame::new(
        GatewayEventField::MessageId,
        &value.message_id,
    )?);
    push_text_frames(&mut frames, &value.text)?;
    Ok(frames)
}

/// Decodes typed Event frames into one normalized inbound message.
///
/// # Errors
///
/// Returns a wire error when fields are missing, duplicated, out of order, or
/// contain invalid text.
pub fn gateway_event_from_frames(
    frames: impl IntoIterator<Item = GatewayEventFrame>,
) -> Result<GatewayInboundMessage, GatewayWireError> {
    let mut channel = None;
    let mut conversation = None;
    let mut thread = None;
    let mut message_id = None;
    let mut text = String::new();
    let mut text_started = false;
    let mut text_complete = false;

    for frame in frames {
        if text_complete {
            return Err(GatewayWireError::InvalidRequest);
        }
        let value = frame.text()?;
        match frame.field {
            GatewayEventField::Channel if channel.is_none() && !text_started => {
                channel = Some(value.to_string());
            }
            GatewayEventField::Conversation if conversation.is_none() && !text_started => {
                conversation = Some(value.to_string());
            }
            GatewayEventField::Thread if thread.is_none() && !text_started => {
                thread = Some(value.to_string());
            }
            GatewayEventField::MessageId if message_id.is_none() && !text_started => {
                message_id = Some(value.to_string());
            }
            GatewayEventField::TextMore => {
                text_started = true;
                text.push_str(value);
            }
            GatewayEventField::TextComplete => {
                text_started = true;
                text.push_str(value);
                text_complete = true;
            }
            _ => return Err(GatewayWireError::InvalidRequest),
        }
    }

    if !text_complete {
        return Err(GatewayWireError::InvalidRequest);
    }
    Ok(GatewayInboundMessage {
        route: GatewayRoute {
            channel: channel.ok_or(GatewayWireError::InvalidRequest)?,
            conversation_id: conversation.ok_or(GatewayWireError::InvalidRequest)?,
            thread_id: thread,
        },
        message_id: message_id.ok_or(GatewayWireError::InvalidRequest)?,
        text,
    })
}

fn push_text_frames(
    frames: &mut Vec<GatewayEventFrame>,
    text: &str,
) -> Result<(), GatewayWireError> {
    if text.as_bytes().contains(&0) {
        return Err(GatewayWireError::EmbeddedNul);
    }
    let chunks = utf8_chunks(text, TEXT_CAPACITY.saturating_sub(1));
    let last = chunks.len().saturating_sub(1);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let field = if index == last {
            GatewayEventField::TextComplete
        } else {
            GatewayEventField::TextMore
        };
        frames.push(GatewayEventFrame::new(field, chunk)?);
    }
    Ok(())
}

fn utf8_chunks(value: &str, capacity: usize) -> Vec<&str> {
    if value.is_empty() {
        return alloc::vec![""];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = core::cmp::min(start.saturating_add(capacity), value.len());
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if end == start {
            break;
        }
        if let Some(chunk) = value.get(start..end) {
            chunks.push(chunk);
        }
        start = end;
    }
    chunks
}
