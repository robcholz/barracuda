use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_event_router::{
    rpc_message, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};
use gateway::{MessageGateway, MessageKind, MessageTarget, SendMessageRequest};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::route::GatewayRoute;
use crate::wire::{GatewaySendReceipt, GatewayText, GatewayWireError};

const TEXT_CAPACITY: usize = 508;

/// Gateway-owned logical request accepted by `gateway.send`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayOutboundMessage {
    /// Destination route and provider conversation identity.
    pub route: GatewayRoute,
    /// Text to deliver.
    pub text: String,
    /// Optional provider message identifier being replied to.
    pub reply_to: Option<String>,
    /// Presentation role of the message; defaults to [`MessageKind::Reply`].
    #[serde(default)]
    pub kind: MessageKind,
}

/// Semantic field carried by one [`GatewaySendRequestFrame`].
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
pub enum GatewaySendField {
    /// Registered message-channel name.
    Channel,
    /// Provider conversation identifier.
    Conversation,
    /// Optional provider thread identifier.
    Thread,
    /// Optional provider message identifier being replied to.
    ReplyTo,
    /// More text belongs to the same outbound message.
    TextMore,
    /// This frame completes the outbound message text.
    TextComplete,
}

/// Wire representation of [`MessageKind`] for `gateway.send`.
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
#[serde(rename_all = "snake_case")]
pub enum GatewayMessageKind {
    /// Primary user-visible reply.
    Reply,
    /// Secondary reasoning text.
    Reasoning,
    /// Tool-execution notice.
    Tool,
    /// Other system notice.
    Notice,
}

impl From<MessageKind> for GatewayMessageKind {
    fn from(value: MessageKind) -> Self {
        match value {
            MessageKind::Reply => Self::Reply,
            MessageKind::Reasoning => Self::Reasoning,
            MessageKind::Tool => Self::Tool,
            MessageKind::Notice => Self::Notice,
        }
    }
}

impl From<GatewayMessageKind> for MessageKind {
    fn from(value: GatewayMessageKind) -> Self {
        match value {
            GatewayMessageKind::Reply => Self::Reply,
            GatewayMessageKind::Reasoning => Self::Reasoning,
            GatewayMessageKind::Tool => Self::Tool,
            GatewayMessageKind::Notice => Self::Notice,
        }
    }
}

/// One typed frame carrying part of a `gateway.send` request.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewaySendRequestFrame {
    text: GatewayText<TEXT_CAPACITY>,
    field: GatewaySendField,
    kind: GatewayMessageKind,
}

impl GatewaySendRequestFrame {
    fn new(
        field: GatewaySendField,
        kind: GatewayMessageKind,
        text: &str,
    ) -> Result<Self, GatewayWireError> {
        Ok(Self {
            text: GatewayText::new(text)?,
            field,
            kind,
        })
    }

    fn text(&self) -> Result<&str, GatewayWireError> {
        self.text.as_str()
    }
}

/// Business failure returned by `gateway.send`.
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
#[serde(rename_all = "snake_case")]
pub enum GatewaySendError {
    /// The streamed request did not follow the `gateway.send` contract.
    InvalidRequest,
    /// No provider is registered for the requested channel.
    UnknownChannel,
    /// The selected provider rejected or failed the delivery.
    Delivery,
    /// The provider receipt could not fit in the Gateway response contract.
    InvalidReceipt,
}

/// Outbound text-message delivery RPC.
pub struct GatewaySend;

impl RpcMethod for GatewaySend {
    const ADDRESS: &'static str = "gateway.send";
    type Request = GatewaySendRequestFrame;
    type Response = GatewaySendReceipt;
    type Error = GatewaySendError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`GatewaySend`].
pub fn gateway_send_handler(gateway: Rc<MessageGateway>) -> impl RpcHandler<GatewaySend> {
    move |_context, frames: RpcStream<RpcFrame<GatewaySendRequestFrame>>| {
        let gateway = Rc::clone(&gateway);
        async move {
            let message = match collect_request(frames).await? {
                Ok(message) => message,
                Err(_error) => return Ok(Err(GatewaySendError::InvalidRequest)),
            };
            let mut target =
                MessageTarget::new(message.route.channel, message.route.conversation_id);
            target.thread_id = message.route.thread_id;
            let mut request = SendMessageRequest::text(target, message.text);
            request.reply_to = message.reply_to;
            request.kind = message.kind;
            let receipt = match gateway.send_message(request).await {
                Ok(receipt) => receipt,
                Err(gateway::GatewayError::UnknownChannel { .. }) => {
                    return Ok(Err(GatewaySendError::UnknownChannel));
                }
                Err(_error) => return Ok(Err(GatewaySendError::Delivery)),
            };
            match GatewaySendReceipt::new(&receipt.message_id) {
                Ok(receipt) => Ok(Ok(receipt)),
                Err(_error) => Ok(Err(GatewaySendError::InvalidReceipt)),
            }
        }
    }
}

/// Encodes one logical text send into typed request frames.
///
/// # Errors
///
/// Returns a wire error when route metadata cannot fit in one frame or message
/// text contains a NUL byte.
pub fn frames_from_gateway_send(
    value: &GatewayOutboundMessage,
) -> Result<Vec<GatewaySendRequestFrame>, GatewayWireError> {
    let kind = value.kind.into();
    let mut frames = Vec::new();
    frames.push(GatewaySendRequestFrame::new(
        GatewaySendField::Channel,
        kind,
        &value.route.channel,
    )?);
    frames.push(GatewaySendRequestFrame::new(
        GatewaySendField::Conversation,
        kind,
        &value.route.conversation_id,
    )?);
    if let Some(thread_id) = &value.route.thread_id {
        frames.push(GatewaySendRequestFrame::new(
            GatewaySendField::Thread,
            kind,
            thread_id,
        )?);
    }
    if let Some(reply_to) = &value.reply_to {
        frames.push(GatewaySendRequestFrame::new(
            GatewaySendField::ReplyTo,
            kind,
            reply_to,
        )?);
    }
    push_text_frames(&mut frames, kind, &value.text)?;
    Ok(frames)
}

/// Decodes typed `gateway.send` request frames into their logical DTO.
///
/// # Errors
///
/// Returns a wire error when fields are missing, duplicated, out of order, or
/// contain invalid text.
pub fn gateway_send_from_frames(
    frames: impl IntoIterator<Item = GatewaySendRequestFrame>,
) -> Result<GatewayOutboundMessage, GatewayWireError> {
    decode_frames(frames)
}

async fn collect_request(
    mut frames: RpcStream<RpcFrame<GatewaySendRequestFrame>>,
) -> barracuda_event_router::RpcResult<Result<GatewayOutboundMessage, GatewayWireError>> {
    let mut collected = Vec::new();
    while let Some(frame) = frames.next().await {
        collected.push(*frame?.view()?);
    }
    Ok(decode_frames(collected))
}

fn decode_frames(
    frames: impl IntoIterator<Item = GatewaySendRequestFrame>,
) -> Result<GatewayOutboundMessage, GatewayWireError> {
    let mut channel = None;
    let mut conversation = None;
    let mut thread = None;
    let mut reply_to = None;
    let mut text = String::new();
    let mut kind = None;
    let mut text_started = false;
    let mut text_complete = false;

    for frame in frames {
        if text_complete || kind.is_some_and(|expected| expected != frame.kind) {
            return Err(GatewayWireError::InvalidRequest);
        }
        kind = Some(frame.kind);
        let value = frame.text()?;
        match frame.field {
            GatewaySendField::Channel if channel.is_none() && !text_started => {
                channel = Some(value.to_string());
            }
            GatewaySendField::Conversation if conversation.is_none() && !text_started => {
                conversation = Some(value.to_string());
            }
            GatewaySendField::Thread if thread.is_none() && !text_started => {
                thread = Some(value.to_string());
            }
            GatewaySendField::ReplyTo if reply_to.is_none() && !text_started => {
                reply_to = Some(value.to_string());
            }
            GatewaySendField::TextMore => {
                text_started = true;
                text.push_str(value);
            }
            GatewaySendField::TextComplete => {
                text_started = true;
                text.push_str(value);
                text_complete = true;
            }
            _ => return Err(GatewayWireError::InvalidRequest),
        }
    }

    let kind = kind.ok_or(GatewayWireError::InvalidRequest)?;
    if !text_complete {
        return Err(GatewayWireError::InvalidRequest);
    }
    Ok(GatewayOutboundMessage {
        route: GatewayRoute {
            channel: channel.ok_or(GatewayWireError::InvalidRequest)?,
            conversation_id: conversation.ok_or(GatewayWireError::InvalidRequest)?,
            thread_id: thread,
        },
        text,
        reply_to,
        kind: kind.into(),
    })
}

fn push_text_frames(
    frames: &mut Vec<GatewaySendRequestFrame>,
    kind: GatewayMessageKind,
    text: &str,
) -> Result<(), GatewayWireError> {
    if text.as_bytes().contains(&0) {
        return Err(GatewayWireError::EmbeddedNul);
    }
    let chunks = utf8_chunks(text, TEXT_CAPACITY.saturating_sub(1));
    let last = chunks.len().saturating_sub(1);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let field = if index == last {
            GatewaySendField::TextComplete
        } else {
            GatewaySendField::TextMore
        };
        frames.push(GatewaySendRequestFrame::new(field, kind, chunk)?);
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
