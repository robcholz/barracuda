use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use async_channel::{Receiver, Sender, TrySendError};
use barracuda_workflow_plugin::{Event, WorkflowService};
use futures_lite::stream;
use gateway::{
    BinaryBody, BinaryChunk, MediaKind, MessageGateway, MessageTarget, SendMediaRequest,
};
use serde::{Deserialize, Serialize};

use crate::component::STREAM_WORKERS;
use crate::json::{
    map_gateway_error, valid_required, valid_stream_id, GatewayAccepted, GatewayOperationError,
};

const CHUNK_QUEUE_CAPACITY: usize = 2;

/// Media kind selected by a Gateway media stream.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    fn from(kind: GatewayMediaKind) -> Self {
        match kind {
            GatewayMediaKind::File => Self::File,
            GatewayMediaKind::Image => Self::Image,
            GatewayMediaKind::Audio => Self::Audio,
            GatewayMediaKind::Video => Self::Video,
        }
    }
}

/// One command for an outbound binary media stream.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewaySendMediaRequest {
    /// Opens a stream and reserves one media worker.
    Start {
        /// Stable caller-owned stream identifier.
        stream_id: String,
        /// Must be zero for stream creation.
        sequence: u32,
        /// Registered provider channel.
        channel: String,
        /// Provider conversation identifier.
        conversation_id: String,
        /// Optional provider thread identifier.
        thread_id: Option<String>,
        /// Attachment presentation kind.
        kind: GatewayMediaKind,
        /// Optional attachment filename.
        filename: Option<String>,
        /// Optional MIME type.
        mime_type: Option<String>,
        /// Optional caption.
        caption: Option<String>,
        /// Optional provider message being replied to.
        reply_to: Option<String>,
    },
    /// Appends one base64-encoded binary chunk.
    Chunk {
        /// Stable caller-owned stream identifier.
        stream_id: String,
        /// Next expected stream sequence.
        sequence: u32,
        /// Canonical base64 content.
        content_base64: String,
    },
    /// Closes the binary stream.
    Finish {
        /// Stable caller-owned stream identifier.
        stream_id: String,
        /// Next expected stream sequence.
        sequence: u32,
    },
}

/// Terminal Workflow Event for one accepted outbound media stream.
pub struct GatewaySendMediaFinished;

impl Event for GatewaySendMediaFinished {
    const ID: &'static str = "gateway.send_media.finished";
}

enum MediaCommand {
    Chunk(Vec<u8>),
    Finish(u32),
}

struct MediaSession {
    next_sequence: Cell<u32>,
    commands: Sender<MediaCommand>,
}

pub(crate) struct MediaJob {
    stream_id: String,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
    kind: MediaKind,
    request: SendMediaRequest,
}

#[derive(Default)]
pub(crate) struct MediaSessions {
    entries: RefCell<BTreeMap<String, Rc<MediaSession>>>,
}

impl MediaSessions {
    fn remove(&self, stream_id: &str) {
        self.entries.borrow_mut().remove(stream_id);
    }

    fn prepare(
        &self,
        stream_id: &str,
        sequence: u32,
    ) -> Result<Rc<MediaSession>, GatewayOperationError> {
        let session = self
            .entries
            .borrow()
            .get(stream_id)
            .cloned()
            .ok_or(GatewayOperationError::UnknownStream)?;
        if sequence != session.next_sequence.get() {
            return Err(GatewayOperationError::OutOfOrder);
        }
        Ok(session)
    }
}

pub(crate) fn accept_media(
    sessions: &MediaSessions,
    jobs: &Sender<MediaJob>,
    request: GatewaySendMediaRequest,
) -> Result<GatewayAccepted, GatewayOperationError> {
    match request {
        GatewaySendMediaRequest::Start {
            stream_id,
            sequence,
            channel,
            conversation_id,
            thread_id,
            kind,
            filename,
            mime_type,
            caption,
            reply_to,
        } => {
            if sequence != 0
                || !valid_stream_id(&stream_id)
                || !valid_required(&channel)
                || !valid_required(&conversation_id)
            {
                return Err(GatewayOperationError::InvalidRequest);
            }
            if sessions.entries.borrow().contains_key(&stream_id) {
                return Err(GatewayOperationError::DuplicateStream);
            }
            if sessions.entries.borrow().len() >= STREAM_WORKERS {
                return Err(GatewayOperationError::Busy);
            }
            let (commands, receiver) = async_channel::bounded(CHUNK_QUEUE_CAPACITY);
            let terminal_sequence = Rc::new(RefCell::new(None));
            let mut target = MessageTarget::new(channel, conversation_id);
            target.thread_id = thread_id;
            let request = SendMediaRequest {
                target,
                body: BinaryBody::Stream(binary_stream(receiver, Rc::clone(&terminal_sequence))),
                filename,
                mime_type,
                caption,
                reply_to,
            };
            jobs.try_send(MediaJob {
                stream_id: stream_id.clone(),
                terminal_sequence,
                kind: kind.into(),
                request,
            })
            .map_err(|_error| GatewayOperationError::Busy)?;
            sessions.entries.borrow_mut().insert(
                stream_id,
                Rc::new(MediaSession {
                    next_sequence: Cell::new(1),
                    commands,
                }),
            );
            Ok(GatewayAccepted {
                accepted_sequence: u64::from(sequence),
            })
        }
        GatewaySendMediaRequest::Chunk {
            stream_id,
            sequence,
            content_base64,
        } => {
            if !valid_stream_id(&stream_id) {
                return Err(GatewayOperationError::InvalidRequest);
            }
            let bytes = decode_base64(&content_base64)?;
            let session = sessions.prepare(&stream_id, sequence)?;
            match session.commands.try_send(MediaCommand::Chunk(bytes)) {
                Ok(()) => session
                    .next_sequence
                    .set(session.next_sequence.get().saturating_add(1)),
                Err(TrySendError::Full(_command)) => return Err(GatewayOperationError::Busy),
                Err(TrySendError::Closed(_command)) => {
                    sessions.remove(&stream_id);
                    return Err(GatewayOperationError::UnknownStream);
                }
            }
            Ok(GatewayAccepted {
                accepted_sequence: u64::from(sequence),
            })
        }
        GatewaySendMediaRequest::Finish {
            stream_id,
            sequence,
        } => {
            if !valid_stream_id(&stream_id) {
                return Err(GatewayOperationError::InvalidRequest);
            }
            let session = sessions.prepare(&stream_id, sequence)?;
            match session.commands.try_send(MediaCommand::Finish(sequence)) {
                Ok(()) => sessions.remove(&stream_id),
                Err(TrySendError::Full(_command)) => return Err(GatewayOperationError::Busy),
                Err(TrySendError::Closed(_command)) => {
                    sessions.remove(&stream_id);
                    return Err(GatewayOperationError::UnknownStream);
                }
            }
            Ok(GatewayAccepted {
                accepted_sequence: u64::from(sequence),
            })
        }
    }
}

pub(crate) async fn deliver_media_stream(
    gateway: &MessageGateway,
    sessions: &MediaSessions,
    workflow: &WorkflowService,
    job: MediaJob,
) {
    let MediaJob {
        stream_id,
        terminal_sequence,
        kind,
        request,
    } = job;
    let result = match kind {
        MediaKind::File => gateway.send_file(request).await,
        MediaKind::Image => gateway.send_image(request).await,
        MediaKind::Audio => gateway.send_audio(request).await,
        MediaKind::Video => gateway.send_video(request).await,
    };
    sessions.remove(&stream_id);
    let sequence = terminal_sequence.borrow().unwrap_or_default();
    let terminal = match result {
        Ok(receipt) if terminal_sequence.borrow().is_some() => MediaTerminalEvent {
            stream_id,
            sequence,
            outcome: "completed",
            message_id: Some(receipt.message_id),
            error: None,
        },
        Ok(_receipt) => MediaTerminalEvent {
            stream_id,
            sequence,
            outcome: "failed",
            message_id: None,
            error: Some(GatewayOperationError::Delivery),
        },
        Err(error) => MediaTerminalEvent {
            stream_id,
            sequence,
            outcome: "failed",
            message_id: None,
            error: Some(map_gateway_error(&error)),
        },
    };
    match serde_json::to_value(&terminal) {
        Ok(value) => {
            if let Err(error) = workflow.emit::<GatewaySendMediaFinished>(value) {
                log::error!("IMessage Gateway failed to emit media terminal Event: {error}");
            }
        }
        Err(error) => {
            log::error!("IMessage Gateway failed to serialize media terminal Event: {error}");
        }
    }
}

fn binary_stream(
    commands: Receiver<MediaCommand>,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
) -> gateway::BinaryStream {
    Box::pin(stream::unfold(
        (commands, terminal_sequence),
        |(commands, terminal_sequence)| async move {
            match commands.recv().await {
                Ok(MediaCommand::Chunk(bytes)) => {
                    Some((Ok(BinaryChunk::from(bytes)), (commands, terminal_sequence)))
                }
                Ok(MediaCommand::Finish(sequence)) => {
                    terminal_sequence.replace(Some(sequence));
                    None
                }
                Err(_closed) => None,
            }
        },
    ))
}

fn decode_base64(encoded: &str) -> Result<Vec<u8>, GatewayOperationError> {
    if !encoded.len().is_multiple_of(4) {
        return Err(GatewayOperationError::InvalidRequest);
    }
    let mut output = Vec::with_capacity(encoded.len() / 4 * 3);
    let group_count = encoded.len() / 4;
    for (index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
        let a = decode_base64_byte(*chunk.first().ok_or(GatewayOperationError::InvalidRequest)?)?;
        let b = decode_base64_byte(*chunk.get(1).ok_or(GatewayOperationError::InvalidRequest)?)?;
        let c = *chunk.get(2).ok_or(GatewayOperationError::InvalidRequest)?;
        let d = *chunk.get(3).ok_or(GatewayOperationError::InvalidRequest)?;
        let is_last = index.saturating_add(1) == group_count;
        if c == b'=' && (!is_last || d != b'=') || d == b'=' && !is_last {
            return Err(GatewayOperationError::InvalidRequest);
        }
        let c_value = if c == b'=' { 0 } else { decode_base64_byte(c)? };
        let d_value = if d == b'=' { 0 } else { decode_base64_byte(d)? };
        if c == b'=' && b & 0x0f != 0 || d == b'=' && c_value & 0x03 != 0 {
            return Err(GatewayOperationError::InvalidRequest);
        }
        let word = (u32::from(a) << 18)
            | (u32::from(b) << 12)
            | (u32::from(c_value) << 6)
            | u32::from(d_value);
        output.push(((word >> 16) & 0xff) as u8);
        if c != b'=' {
            output.push(((word >> 8) & 0xff) as u8);
        }
        if d != b'=' {
            output.push((word & 0xff) as u8);
        }
    }
    Ok(output)
}

fn decode_base64_byte(byte: u8) -> Result<u8, GatewayOperationError> {
    match byte {
        b'A'..=b'Z' => byte
            .checked_sub(b'A')
            .ok_or(GatewayOperationError::InvalidRequest),
        b'a'..=b'z' => byte
            .checked_sub(b'a')
            .and_then(|value| value.checked_add(26))
            .ok_or(GatewayOperationError::InvalidRequest),
        b'0'..=b'9' => byte
            .checked_sub(b'0')
            .and_then(|value| value.checked_add(52))
            .ok_or(GatewayOperationError::InvalidRequest),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(GatewayOperationError::InvalidRequest),
    }
}

#[derive(Serialize)]
struct MediaTerminalEvent {
    stream_id: String,
    sequence: u32,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<GatewayOperationError>,
}
