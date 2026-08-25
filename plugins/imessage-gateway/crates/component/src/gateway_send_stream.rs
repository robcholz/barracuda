use alloc::{boxed::Box, format, rc::Rc, string::ToString, vec::Vec};

use barracuda_event_router::{
    rpc_message, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};
use futures_lite::stream;
use gateway::{
    MessageGateway, MessageTarget, SendStreamField, SendStreamFrame, SendStreamRequest,
    StreamBoundary,
};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::route::GatewayRoute;
use crate::wire::{GatewaySendReceipt, GatewayText, GatewayWireError};

const TEXT_CAPACITY: usize = 510;

/// One logical full Gateway stream used by typed callers and tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayOutboundStream {
    /// Destination route and provider conversation identity.
    pub route: GatewayRoute,
    /// Optional provider message being replied to.
    pub reply_to: Option<alloc::string::String>,
    /// Ordered primary-text and extra-content frames.
    pub frames: Vec<SendStreamFrame>,
}

/// Semantic field carried by one [`GatewaySendStreamRequestFrame`].
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
pub enum GatewaySendStreamField {
    /// Registered message-channel name.
    Channel,
    /// Provider conversation identifier.
    Conversation,
    /// Optional provider thread identifier.
    Thread,
    /// Optional provider message identifier being replied to.
    ReplyTo,
    /// More primary text follows for the current block.
    TextMore,
    /// Completes the current primary text block.
    TextComplete,
    /// More reasoning text follows.
    ReasoningMore,
    /// Completes the current reasoning block.
    ReasoningComplete,
    /// More effect-result text follows.
    EffectResultMore,
    /// Completes the current effect-result block.
    EffectResultComplete,
    /// More notice text follows.
    NoticeMore,
    /// Completes the current notice.
    NoticeComplete,
    /// More generic event metadata follows.
    EventMore,
    /// Completes one generic event metadata value.
    EventComplete,
    /// Starts one structured tool result.
    ToolResultStart,
    /// More tool-call identifier text follows.
    ToolCallIdMore,
    /// Completes the tool-call identifier.
    ToolCallIdComplete,
    /// More tool-name text follows.
    ToolNameMore,
    /// Completes the tool name.
    ToolNameComplete,
    /// More tool-arguments JSON follows.
    ToolArgumentsMore,
    /// Completes the tool arguments JSON.
    ToolArgumentsComplete,
    /// More tool-output text follows.
    ToolOutputMore,
    /// Completes the tool output.
    ToolOutputComplete,
    /// Marks successful tool completion.
    ToolSucceeded,
    /// Marks failed tool completion.
    ToolFailed,
    /// Ends one structured tool result.
    ToolResultEnd,
}

/// One typed frame carried by `gateway.send_stream`.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewaySendStreamRequestFrame {
    value: GatewayText<TEXT_CAPACITY>,
    field: GatewaySendStreamField,
}

impl GatewaySendStreamRequestFrame {
    fn new(field: GatewaySendStreamField, value: &str) -> Result<Self, GatewayWireError> {
        Ok(Self {
            value: GatewayText::new(value)?,
            field,
        })
    }

    fn value(&self) -> Result<&str, GatewayWireError> {
        self.value.as_str()
    }

    /// Returns the semantic field carried by this wire frame.
    #[must_use]
    pub const fn field(&self) -> GatewaySendStreamField {
        self.field
    }
}

/// Business failure returned by `gateway.send_stream`.
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
pub enum GatewaySendStreamError {
    /// The request stream did not follow the field protocol.
    InvalidRequest,
    /// No provider is registered for the requested channel.
    UnknownChannel,
    /// The selected provider rejected or failed the delivery.
    Delivery,
    /// The provider receipt could not fit in the Gateway response contract.
    InvalidReceipt,
}

/// Sends one full primary-text stream with optional extra frames.
pub struct GatewaySendStream;

impl RpcMethod for GatewaySendStream {
    const ADDRESS: &'static str = "gateway.send_stream";
    type Request = GatewaySendStreamRequestFrame;
    type Response = GatewaySendReceipt;
    type Error = GatewaySendStreamError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`GatewaySendStream`].
pub fn gateway_send_stream_handler(
    gateway: Rc<MessageGateway>,
) -> impl RpcHandler<GatewaySendStream> {
    move |_context, frames: RpcStream<RpcFrame<GatewaySendStreamRequestFrame>>| {
        let gateway = Rc::clone(&gateway);
        async move {
            let (target, reply_to, frames) = match split_metadata(frames).await? {
                Ok(request) => request,
                Err(_error) => return Ok(Err(GatewaySendStreamError::InvalidRequest)),
            };
            let request = SendStreamRequest {
                target,
                frames,
                reply_to,
            };
            let receipt = match gateway.send_stream(request).await {
                Ok(receipt) => receipt,
                Err(gateway::GatewayError::UnknownChannel { .. }) => {
                    return Ok(Err(GatewaySendStreamError::UnknownChannel));
                }
                Err(_error) => return Ok(Err(GatewaySendStreamError::Delivery)),
            };
            match GatewaySendReceipt::new(&receipt.message_id) {
                Ok(receipt) => Ok(Ok(receipt)),
                Err(_error) => Ok(Err(GatewaySendStreamError::InvalidReceipt)),
            }
        }
    }
}

/// Encodes one logical full send stream into typed RPC frames.
///
/// # Errors
///
/// Returns a wire error when route metadata or content cannot be represented.
pub fn frames_from_gateway_send_stream(
    value: &GatewayOutboundStream,
) -> Result<Vec<GatewaySendStreamRequestFrame>, GatewayWireError> {
    let mut frames = Vec::new();
    frames.push(GatewaySendStreamRequestFrame::new(
        GatewaySendStreamField::Channel,
        &value.route.channel,
    )?);
    frames.push(GatewaySendStreamRequestFrame::new(
        GatewaySendStreamField::Conversation,
        &value.route.conversation_id,
    )?);
    if let Some(thread) = &value.route.thread_id {
        frames.push(GatewaySendStreamRequestFrame::new(
            GatewaySendStreamField::Thread,
            thread,
        )?);
    }
    if let Some(reply_to) = &value.reply_to {
        frames.push(GatewaySendStreamRequestFrame::new(
            GatewaySendStreamField::ReplyTo,
            reply_to,
        )?);
    }
    for content in &value.frames {
        push_content_frames(&mut frames, content)?;
    }
    Ok(frames)
}

/// Encodes one logical content frame into one or more RPC wire frames.
///
/// # Errors
///
/// Returns a wire error when the content contains an embedded NUL byte.
pub fn frames_from_gateway_stream_frame(
    content: &SendStreamFrame,
) -> Result<Vec<GatewaySendStreamRequestFrame>, GatewayWireError> {
    let mut frames = Vec::new();
    push_content_frames(&mut frames, content)?;
    Ok(frames)
}

/// Decodes typed `gateway.send_stream` frames into their logical form.
///
/// # Errors
///
/// Returns a wire error when route fields are missing, duplicated, or out of order.
pub fn gateway_send_stream_from_frames(
    frames: impl IntoIterator<Item = GatewaySendStreamRequestFrame>,
) -> Result<GatewayOutboundStream, GatewayWireError> {
    decode_frames(frames)
}

async fn split_metadata(
    mut frames: RpcStream<RpcFrame<GatewaySendStreamRequestFrame>>,
) -> barracuda_event_router::RpcResult<
    Result<
        (
            MessageTarget,
            Option<alloc::string::String>,
            gateway::SendStream,
        ),
        GatewayWireError,
    >,
> {
    let mut channel = None;
    let mut conversation = None;
    let mut thread = None;
    let mut reply_to = None;

    while let Some(frame) = frames.next().await {
        let frame = *frame?.view()?;
        let value = match frame.value() {
            Ok(value) => value.to_string(),
            Err(error) => return Ok(Err(error)),
        };
        match frame.field {
            GatewaySendStreamField::Channel if channel.is_none() => channel = Some(value),
            GatewaySendStreamField::Conversation if conversation.is_none() => {
                conversation = Some(value);
            }
            GatewaySendStreamField::Thread if thread.is_none() => thread = Some(value),
            GatewaySendStreamField::ReplyTo if reply_to.is_none() => reply_to = Some(value),
            GatewaySendStreamField::Channel
            | GatewaySendStreamField::Conversation
            | GatewaySendStreamField::Thread
            | GatewaySendStreamField::ReplyTo => {
                return Ok(Err(GatewayWireError::InvalidRequest));
            }
            _ => {
                let Some(channel) = channel else {
                    return Ok(Err(GatewayWireError::InvalidRequest));
                };
                let Some(conversation) = conversation else {
                    return Ok(Err(GatewayWireError::InvalidRequest));
                };
                let mut target = MessageTarget::new(channel, conversation);
                target.thread_id = thread;
                let content = content_stream(frame, frames);
                return Ok(Ok((target, reply_to, Box::pin(content))));
            }
        }
    }

    Ok(Err(GatewayWireError::InvalidRequest))
}

fn content_stream(
    first: GatewaySendStreamRequestFrame,
    frames: RpcStream<RpcFrame<GatewaySendStreamRequestFrame>>,
) -> impl futures_lite::Stream<Item = Result<SendStreamFrame, gateway::StreamError>> {
    stream::unfold(
        (Some(first), frames),
        |(mut first, mut frames)| async move {
            let decoded = if let Some(frame) = first.take() {
                decode_content_frame(frame)
            } else {
                match frames.next().await {
                    Some(Ok(frame)) => match frame.view() {
                        Ok(frame) => decode_content_frame(*frame),
                        Err(error) => Err(gateway::StreamError::failed(format!(
                            "invalid gateway stream frame: {error}"
                        ))),
                    },
                    Some(Err(error)) => Err(gateway::StreamError::failed(format!(
                        "gateway stream transport failed: {error}"
                    ))),
                    None => return None,
                }
            };
            Some((decoded, (first, frames)))
        },
    )
}

fn decode_content_frame(
    frame: GatewaySendStreamRequestFrame,
) -> Result<SendStreamFrame, gateway::StreamError> {
    let value = frame
        .value()
        .map_err(|error| gateway::StreamError::failed(format!("invalid stream text: {error}")))?;
    let (field, boundary) = content_field(frame.field)
        .map_err(|error| gateway::StreamError::failed(format!("invalid stream field: {error}")))?;
    Ok(SendStreamFrame::new(field, boundary, value))
}

fn decode_frames(
    frames: impl IntoIterator<Item = GatewaySendStreamRequestFrame>,
) -> Result<GatewayOutboundStream, GatewayWireError> {
    let mut channel = None;
    let mut conversation = None;
    let mut thread = None;
    let mut reply_to = None;
    let mut content = Vec::new();
    let mut content_started = false;

    for frame in frames {
        let value = frame.value()?;
        match frame.field {
            GatewaySendStreamField::Channel if channel.is_none() && !content_started => {
                channel = Some(value.to_string());
            }
            GatewaySendStreamField::Conversation if conversation.is_none() && !content_started => {
                conversation = Some(value.to_string());
            }
            GatewaySendStreamField::Thread if thread.is_none() && !content_started => {
                thread = Some(value.to_string());
            }
            GatewaySendStreamField::ReplyTo if reply_to.is_none() && !content_started => {
                reply_to = Some(value.to_string());
            }
            field => {
                content_started = true;
                let (field, boundary) = content_field(field)?;
                content.push(SendStreamFrame::new(field, boundary, value));
            }
        }
    }

    Ok(GatewayOutboundStream {
        route: GatewayRoute {
            channel: channel.ok_or(GatewayWireError::InvalidRequest)?,
            conversation_id: conversation.ok_or(GatewayWireError::InvalidRequest)?,
            thread_id: thread,
        },
        reply_to,
        frames: content,
    })
}

fn push_content_frames(
    frames: &mut Vec<GatewaySendStreamRequestFrame>,
    content: &SendStreamFrame,
) -> Result<(), GatewayWireError> {
    if content.text.as_bytes().contains(&0) {
        return Err(GatewayWireError::EmbeddedNul);
    }
    let chunks = utf8_chunks(&content.text, TEXT_CAPACITY.saturating_sub(1));
    let last = chunks.len().saturating_sub(1);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let boundary = if index == last {
            content.boundary
        } else {
            StreamBoundary::More
        };
        frames.push(GatewaySendStreamRequestFrame::new(
            wire_field(content.field, boundary),
            chunk,
        )?);
    }
    Ok(())
}

fn wire_field(field: SendStreamField, boundary: StreamBoundary) -> GatewaySendStreamField {
    use GatewaySendStreamField as Wire;
    use SendStreamField as Domain;
    use StreamBoundary::{Complete, More};
    match (field, boundary) {
        (Domain::Text, More) => Wire::TextMore,
        (Domain::Text, Complete) => Wire::TextComplete,
        (Domain::Reasoning, More) => Wire::ReasoningMore,
        (Domain::Reasoning, Complete) => Wire::ReasoningComplete,
        (Domain::EffectResult, More) => Wire::EffectResultMore,
        (Domain::EffectResult, Complete) => Wire::EffectResultComplete,
        (Domain::Notice, More) => Wire::NoticeMore,
        (Domain::Notice, Complete) => Wire::NoticeComplete,
        (Domain::Event, More) => Wire::EventMore,
        (Domain::Event, Complete) => Wire::EventComplete,
        (Domain::ToolResultStart, _) => Wire::ToolResultStart,
        (Domain::ToolCallId, More) => Wire::ToolCallIdMore,
        (Domain::ToolCallId, Complete) => Wire::ToolCallIdComplete,
        (Domain::ToolName, More) => Wire::ToolNameMore,
        (Domain::ToolName, Complete) => Wire::ToolNameComplete,
        (Domain::ToolArguments, More) => Wire::ToolArgumentsMore,
        (Domain::ToolArguments, Complete) => Wire::ToolArgumentsComplete,
        (Domain::ToolOutput, More) => Wire::ToolOutputMore,
        (Domain::ToolOutput, Complete) => Wire::ToolOutputComplete,
        (Domain::ToolSucceeded, _) => Wire::ToolSucceeded,
        (Domain::ToolFailed, _) => Wire::ToolFailed,
        (Domain::ToolResultEnd, _) => Wire::ToolResultEnd,
    }
}

fn content_field(
    field: GatewaySendStreamField,
) -> Result<(SendStreamField, StreamBoundary), GatewayWireError> {
    use GatewaySendStreamField as Wire;
    use SendStreamField as Domain;
    use StreamBoundary::{Complete, More};
    let value = match field {
        Wire::TextMore => (Domain::Text, More),
        Wire::TextComplete => (Domain::Text, Complete),
        Wire::ReasoningMore => (Domain::Reasoning, More),
        Wire::ReasoningComplete => (Domain::Reasoning, Complete),
        Wire::EffectResultMore => (Domain::EffectResult, More),
        Wire::EffectResultComplete => (Domain::EffectResult, Complete),
        Wire::NoticeMore => (Domain::Notice, More),
        Wire::NoticeComplete => (Domain::Notice, Complete),
        Wire::EventMore => (Domain::Event, More),
        Wire::EventComplete => (Domain::Event, Complete),
        Wire::ToolResultStart => (Domain::ToolResultStart, Complete),
        Wire::ToolCallIdMore => (Domain::ToolCallId, More),
        Wire::ToolCallIdComplete => (Domain::ToolCallId, Complete),
        Wire::ToolNameMore => (Domain::ToolName, More),
        Wire::ToolNameComplete => (Domain::ToolName, Complete),
        Wire::ToolArgumentsMore => (Domain::ToolArguments, More),
        Wire::ToolArgumentsComplete => (Domain::ToolArguments, Complete),
        Wire::ToolOutputMore => (Domain::ToolOutput, More),
        Wire::ToolOutputComplete => (Domain::ToolOutput, Complete),
        Wire::ToolSucceeded => (Domain::ToolSucceeded, Complete),
        Wire::ToolFailed => (Domain::ToolFailed, Complete),
        Wire::ToolResultEnd => (Domain::ToolResultEnd, Complete),
        Wire::Channel | Wire::Conversation | Wire::Thread | Wire::ReplyTo => {
            return Err(GatewayWireError::InvalidRequest);
        }
    };
    Ok(value)
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
