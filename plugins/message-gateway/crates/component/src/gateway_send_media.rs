use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_event_router::{
    rpc_message, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};
use gateway::{BinaryBody, ChannelError, GatewayError, MediaKind, MessageGateway, MessageTarget};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::route::GatewayRoute;
use crate::wire::{GatewaySendReceipt, GatewayText, GatewayWireError};

const METADATA_CAPACITY: usize = 252;
const MEDIA_CHUNK_CAPACITY: usize = 254;

/// Gateway-owned logical request encoded by `gateway.send_media` frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayOutboundMedia {
    /// Destination route and provider conversation identity.
    pub route: GatewayRoute,
    /// Kind of media being delivered.
    pub kind: GatewayMediaKind,
    /// Optional provider-visible filename.
    pub filename: Option<String>,
    /// Optional MIME type.
    pub mime_type: Option<String>,
    /// Optional text rendered with the media.
    pub caption: Option<String>,
    /// Optional provider message identifier being replied to.
    pub reply_to: Option<String>,
    /// Complete binary body encoded into streaming request frames.
    pub bytes: Vec<u8>,
}

/// Kind of media delivered by `gateway.send_media`.
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
pub enum GatewayMediaKind {
    /// Generic file attachment.
    File,
    /// Image attachment.
    Image,
    /// Audio attachment.
    Audio,
    /// Video attachment.
    Video,
}

impl From<GatewayMediaKind> for MediaKind {
    fn from(value: GatewayMediaKind) -> Self {
        match value {
            GatewayMediaKind::File => Self::File,
            GatewayMediaKind::Image => Self::Image,
            GatewayMediaKind::Audio => Self::Audio,
            GatewayMediaKind::Video => Self::Video,
        }
    }
}

/// Semantic field carried by one [`GatewaySendMediaRequestFrame`].
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
pub enum GatewaySendMediaField {
    /// Registered message-channel name.
    Channel,
    /// Provider conversation identifier.
    Conversation,
    /// Optional provider thread identifier.
    Thread,
    /// Optional provider-visible filename.
    Filename,
    /// Optional MIME type.
    MimeType,
    /// Optional text rendered with the media.
    Caption,
    /// Optional provider message identifier being replied to.
    ReplyTo,
    /// Opaque bytes belonging to the media body.
    Body,
}

/// One bounded opaque binary chunk carried by `gateway.send_media`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub struct GatewayMediaChunk {
    length: u16,
    bytes: [u8; MEDIA_CHUNK_CAPACITY],
}

impl GatewayMediaChunk {
    fn empty() -> Self {
        Self {
            length: 0,
            bytes: [0_u8; MEDIA_CHUNK_CAPACITY],
        }
    }

    fn new(bytes: &[u8]) -> Result<Self, GatewayWireError> {
        let length = u16::try_from(bytes.len()).map_err(|_error| GatewayWireError::InvalidChunk)?;
        let mut chunk = Self::empty();
        chunk
            .bytes
            .get_mut(..bytes.len())
            .ok_or(GatewayWireError::InvalidChunk)?
            .copy_from_slice(bytes);
        chunk.length = length;
        Ok(chunk)
    }

    fn as_slice(&self) -> Result<&[u8], GatewayWireError> {
        self.bytes
            .get(..usize::from(self.length))
            .ok_or(GatewayWireError::InvalidChunk)
    }
}

impl Serialize for GatewayMediaChunk {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.as_slice().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de> Deserialize<'de> for GatewayMediaChunk {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        Self::new(&bytes).map_err(serde::de::Error::custom)
    }
}

/// One typed frame carrying metadata or opaque body bytes for
/// `gateway.send_media`.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewaySendMediaRequestFrame {
    binary: GatewayMediaChunk,
    text: GatewayText<METADATA_CAPACITY>,
    field: GatewaySendMediaField,
    kind: GatewayMediaKind,
}

impl GatewaySendMediaRequestFrame {
    fn metadata(
        field: GatewaySendMediaField,
        kind: GatewayMediaKind,
        value: &str,
    ) -> Result<Self, GatewayWireError> {
        Ok(Self {
            binary: GatewayMediaChunk::empty(),
            text: GatewayText::new(value)?,
            field,
            kind,
        })
    }

    fn body(kind: GatewayMediaKind, bytes: &[u8]) -> Result<Self, GatewayWireError> {
        Ok(Self {
            binary: GatewayMediaChunk::new(bytes)?,
            text: GatewayText::new("")?,
            field: GatewaySendMediaField::Body,
            kind,
        })
    }
}

/// Business failure returned by `gateway.send_media`.
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
pub enum GatewaySendMediaError {
    /// The streamed request did not follow the `gateway.send_media` contract.
    InvalidRequest,
    /// No provider is registered for the requested channel.
    UnknownChannel,
    /// The selected provider does not implement this media kind.
    Unsupported,
    /// The selected provider rejected or failed the delivery.
    Delivery,
    /// The provider receipt could not fit in the Gateway response contract.
    InvalidReceipt,
}

/// Outbound media-delivery RPC.
pub struct GatewaySendMedia;

impl RpcMethod for GatewaySendMedia {
    const ADDRESS: &'static str = "gateway.send_media";
    type Request = GatewaySendMediaRequestFrame;
    type Response = GatewaySendReceipt;
    type Error = GatewaySendMediaError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`GatewaySendMedia`].
pub fn gateway_send_media_handler(
    gateway: Rc<MessageGateway>,
) -> impl RpcHandler<GatewaySendMedia> {
    move |_context, frames: RpcStream<RpcFrame<GatewaySendMediaRequestFrame>>| {
        let gateway = Rc::clone(&gateway);
        async move {
            let message = match collect_request(frames).await? {
                Ok(message) => message,
                Err(_error) => return Ok(Err(GatewaySendMediaError::InvalidRequest)),
            };
            let mut target =
                MessageTarget::new(message.route.channel, message.route.conversation_id);
            target.thread_id = message.route.thread_id;
            let request = gateway::SendMediaRequest {
                target,
                body: BinaryBody::Bytes(message.bytes),
                filename: message.filename,
                mime_type: message.mime_type,
                caption: message.caption,
                reply_to: message.reply_to,
            };
            let delivered = match message.kind {
                GatewayMediaKind::File => gateway.send_file(request).await,
                GatewayMediaKind::Image => gateway.send_image(request).await,
                GatewayMediaKind::Audio => gateway.send_audio(request).await,
                GatewayMediaKind::Video => gateway.send_video(request).await,
            };
            let receipt = match delivered {
                Ok(receipt) => receipt,
                Err(GatewayError::UnknownChannel { .. }) => {
                    return Ok(Err(GatewaySendMediaError::UnknownChannel));
                }
                Err(GatewayError::Channel {
                    source: ChannelError::Unsupported { .. },
                    ..
                }) => return Ok(Err(GatewaySendMediaError::Unsupported)),
                Err(_error) => return Ok(Err(GatewaySendMediaError::Delivery)),
            };
            match GatewaySendReceipt::new(&receipt.message_id) {
                Ok(receipt) => Ok(Ok(receipt)),
                Err(_error) => Ok(Err(GatewaySendMediaError::InvalidReceipt)),
            }
        }
    }
}

/// Encodes one logical media send into typed request frames.
///
/// # Errors
///
/// Returns a wire error when metadata cannot fit in one frame or contains a
/// NUL byte.
pub fn frames_from_gateway_send_media(
    value: &GatewayOutboundMedia,
) -> Result<Vec<GatewaySendMediaRequestFrame>, GatewayWireError> {
    let mut frames = Vec::new();
    frames.push(GatewaySendMediaRequestFrame::metadata(
        GatewaySendMediaField::Channel,
        value.kind,
        &value.route.channel,
    )?);
    frames.push(GatewaySendMediaRequestFrame::metadata(
        GatewaySendMediaField::Conversation,
        value.kind,
        &value.route.conversation_id,
    )?);
    push_optional_metadata(
        &mut frames,
        GatewaySendMediaField::Thread,
        value.kind,
        value.route.thread_id.as_deref(),
    )?;
    push_optional_metadata(
        &mut frames,
        GatewaySendMediaField::Filename,
        value.kind,
        value.filename.as_deref(),
    )?;
    push_optional_metadata(
        &mut frames,
        GatewaySendMediaField::MimeType,
        value.kind,
        value.mime_type.as_deref(),
    )?;
    push_optional_metadata(
        &mut frames,
        GatewaySendMediaField::Caption,
        value.kind,
        value.caption.as_deref(),
    )?;
    push_optional_metadata(
        &mut frames,
        GatewaySendMediaField::ReplyTo,
        value.kind,
        value.reply_to.as_deref(),
    )?;
    if value.bytes.is_empty() {
        frames.push(GatewaySendMediaRequestFrame::body(value.kind, &[])?);
    } else {
        for bytes in value.bytes.chunks(MEDIA_CHUNK_CAPACITY) {
            frames.push(GatewaySendMediaRequestFrame::body(value.kind, bytes)?);
        }
    }
    Ok(frames)
}

/// Decodes typed `gateway.send_media` frames into their logical DTO.
///
/// # Errors
///
/// Returns a wire error when fields are missing, duplicated, or placed after
/// the media body starts.
pub fn gateway_send_media_from_frames(
    frames: impl IntoIterator<Item = GatewaySendMediaRequestFrame>,
) -> Result<GatewayOutboundMedia, GatewayWireError> {
    decode_frames(frames)
}

async fn collect_request(
    mut frames: RpcStream<RpcFrame<GatewaySendMediaRequestFrame>>,
) -> barracuda_event_router::RpcResult<Result<GatewayOutboundMedia, GatewayWireError>> {
    let mut collected = Vec::new();
    while let Some(frame) = frames.next().await {
        collected.push(*frame?.view()?);
    }
    Ok(decode_frames(collected))
}

fn decode_frames(
    frames: impl IntoIterator<Item = GatewaySendMediaRequestFrame>,
) -> Result<GatewayOutboundMedia, GatewayWireError> {
    let mut channel = None;
    let mut conversation = None;
    let mut thread = None;
    let mut filename = None;
    let mut mime_type = None;
    let mut caption = None;
    let mut reply_to = None;
    let mut bytes = Vec::new();
    let mut kind = None;
    let mut body_started = false;

    for frame in frames {
        if kind.is_some_and(|expected| expected != frame.kind) {
            return Err(GatewayWireError::InvalidRequest);
        }
        kind = Some(frame.kind);
        if frame.field == GatewaySendMediaField::Body {
            body_started = true;
            bytes.extend_from_slice(frame.binary.as_slice()?);
            continue;
        }
        if body_started {
            return Err(GatewayWireError::InvalidRequest);
        }
        let value = frame.text.as_str()?.to_string();
        match frame.field {
            GatewaySendMediaField::Channel if channel.is_none() => channel = Some(value),
            GatewaySendMediaField::Conversation if conversation.is_none() => {
                conversation = Some(value);
            }
            GatewaySendMediaField::Thread if thread.is_none() => thread = Some(value),
            GatewaySendMediaField::Filename if filename.is_none() => filename = Some(value),
            GatewaySendMediaField::MimeType if mime_type.is_none() => mime_type = Some(value),
            GatewaySendMediaField::Caption if caption.is_none() => caption = Some(value),
            GatewaySendMediaField::ReplyTo if reply_to.is_none() => reply_to = Some(value),
            _ => return Err(GatewayWireError::InvalidRequest),
        }
    }

    if !body_started {
        return Err(GatewayWireError::InvalidRequest);
    }
    Ok(GatewayOutboundMedia {
        route: GatewayRoute {
            channel: channel.ok_or(GatewayWireError::InvalidRequest)?,
            conversation_id: conversation.ok_or(GatewayWireError::InvalidRequest)?,
            thread_id: thread,
        },
        kind: kind.ok_or(GatewayWireError::InvalidRequest)?,
        filename,
        mime_type,
        caption,
        reply_to,
        bytes,
    })
}

fn push_optional_metadata(
    frames: &mut Vec<GatewaySendMediaRequestFrame>,
    field: GatewaySendMediaField,
    kind: GatewayMediaKind,
    value: Option<&str>,
) -> Result<(), GatewayWireError> {
    if let Some(value) = value {
        frames.push(GatewaySendMediaRequestFrame::metadata(field, kind, value)?);
    }
    Ok(())
}
